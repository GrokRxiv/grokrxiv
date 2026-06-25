//! Tokio scheduler workers for queued AgentHero app runs.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agenthero_dag_runtime::DagNodeStatus;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::app_runs::{self, ClaimedAppRun};

const MAX_ADAPTER_LOG_EVENT_ROWS: usize = 500;
const MAX_ADAPTER_LOG_EVENT_BYTES: usize = 256 * 1024;
const ADAPTER_EVENT_CHANNEL_CAPACITY: usize = 128;
const ADAPTER_LOG_LINE_CHANNEL_CAPACITY: usize = 128;
const MAX_ADAPTER_EVENT_ROWS: usize = 2_000;
const MAX_ADAPTER_EVENT_BYTES: usize = 512 * 1024;
const ADAPTER_PERSIST_JOIN_TIMEOUT: Duration = Duration::from_millis(250);

enum AdapterRunOutcome {
    Response(anyhow::Result<agenthero_agent_runtime::AppAdapterResponse>),
    Cancelled(anyhow::Result<agenthero_agent_runtime::AppAdapterResponse>),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AdapterObservedLifecycle {
    action_started: bool,
    action_completed: bool,
    action_awaiting_approval: bool,
    action_failed: bool,
    action_cancelled: bool,
}

impl AdapterObservedLifecycle {
    fn record_persisted(&mut self, event_type: &str) {
        match event_type {
            "app_action.started" => self.action_started = true,
            "app_action.completed" => self.action_completed = true,
            "app_action.awaiting_approval" => self.action_awaiting_approval = true,
            "app_action.failed" => self.action_failed = true,
            "app_action.cancelled" => self.action_cancelled = true,
            _ => {}
        }
    }

    fn has(&self, event_type: &str) -> bool {
        match event_type {
            "app_action.started" => self.action_started,
            "app_action.completed" => self.action_completed,
            "app_action.awaiting_approval" => self.action_awaiting_approval,
            "app_action.failed" => self.action_failed,
            "app_action.cancelled" => self.action_cancelled,
            _ => false,
        }
    }
}

/// Scheduler configuration.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Number of local worker tasks.
    pub workers: usize,
    /// Poll interval when no queued work is available.
    pub poll_interval: Duration,
}

/// Spawn scheduler workers.
pub fn spawn(pool: PgPool, config: SchedulerConfig) -> Vec<tokio::task::JoinHandle<()>> {
    let pool = Arc::new(pool);
    (0..config.workers.max(1))
        .map(|idx| {
            let pool = Arc::clone(&pool);
            let interval = config.poll_interval;
            tokio::spawn(async move {
                let name = format!("local-{}-{idx}", std::process::id());
                if let Err(err) = worker_loop(pool, name, interval).await {
                    tracing::error!(err = %err, "AgentHero scheduler worker stopped");
                }
            })
        })
        .collect()
}

/// Claim and execute one queued app run, then return.
pub async fn work_once(
    pool: PgPool,
    run_id: Option<Uuid>,
    worker_name: Option<String>,
    stream_stderr: bool,
    debug_logs: bool,
) -> anyhow::Result<Option<Uuid>> {
    let name = worker_name.unwrap_or_else(|| format!("local-once-{}", std::process::id()));
    let worker_id = app_runs::register_worker(&pool, &name).await?;
    app_runs::heartbeat_worker(&pool, worker_id).await?;
    let recovery = app_runs::recover_expired_leases(&pool).await?;
    if recovery.requeued > 0 || recovery.system_failed > 0 {
        tracing::warn!(
            requeued = recovery.requeued,
            system_failed = recovery.system_failed,
            "recovered expired AgentHero app-run leases"
        );
    }
    let claimed = match run_id {
        Some(run_id) => app_runs::claim_run(&pool, worker_id, run_id).await?,
        None => app_runs::claim_next(&pool, worker_id).await?,
    };
    let Some(mut run) = claimed else {
        return Ok(None);
    };
    prepare_app_run_worker_input(
        &mut run.input.input,
        run.id,
        run.dag_run_id,
        run.lease_id,
        stream_stderr,
        debug_logs,
    );
    let claimed_id = run.id;
    execute_claimed(&pool, run).await?;
    Ok(Some(claimed_id))
}

async fn worker_loop(pool: Arc<PgPool>, name: String, interval: Duration) -> anyhow::Result<()> {
    let worker_id = app_runs::register_worker(&pool, &name).await?;
    let mut idle_polls = 0u32;
    loop {
        app_runs::heartbeat_worker(&pool, worker_id).await?;
        let recovery = app_runs::recover_expired_leases(&pool).await?;
        if recovery.requeued > 0 || recovery.system_failed > 0 {
            tracing::warn!(
                requeued = recovery.requeued,
                system_failed = recovery.system_failed,
                "recovered expired AgentHero app-run leases"
            );
        }
        match app_runs::claim_next(&pool, worker_id).await? {
            Some(run) => {
                idle_polls = 0;
                execute_claimed(&pool, run).await?;
            }
            None => {
                idle_polls = idle_polls.saturating_add(1);
                tokio::time::sleep(idle_sleep_duration(interval, idle_polls)).await;
            }
        }
    }
}

async fn execute_claimed(pool: &PgPool, mut run: ClaimedAppRun) -> anyhow::Result<()> {
    prepare_app_run_worker_input(
        &mut run.input.input,
        run.id,
        run.dag_run_id,
        run.lease_id,
        false,
        false,
    );
    let dag_type = claimed_run_dag_type(&run)?;
    app_runs::reserve_dag_run(
        pool,
        run.id,
        run.lease_id,
        run.dag_run_id,
        &dag_type,
        &run.input.input,
    )
    .await?;
    let trace = SchedulerTraceContext::from_claimed_run(&run, &dag_type);
    let idempotency_key = app_run_idempotency_key(run.id);
    let log_path = crate::dag_apps::app_run_log_path(run.id);
    tracing::info!(
        target: "agenthero::scheduler",
        event_type = "app_run.started",
        app_run_id = %trace.app_run_id,
        dag_run_id = %trace.dag_run_id,
        node_id = %trace.node_id,
        attempt = trace.attempt,
        node_kind = %trace.node_kind,
        tool_id = %trace.tool_id,
        manifest_hash = %trace.manifest_hash,
        artifact_id = %trace.artifact_id,
        lease_id = %trace.lease_id,
        status = %trace.status,
        exit_status = trace.exit_status,
        duration_ms = trace.duration_ms,
        worker_id = %trace.worker_id,
        app = %trace.app,
        action = %trace.action,
        dag_type = %trace.dag_type,
        "agenthero scheduler event"
    );
    append_app_run_log_event(
        &log_path,
        "info",
        "app_run.started",
        &format!(
            "app={} action={} attempt={}",
            run.app_id, run.action_id, run.attempt
        ),
    );
    app_runs::insert_event(
        pool,
        run.id,
        "info",
        "app_run.started",
        Some("app run started"),
        app_run_started_event_payload(&run, &dag_type, &idempotency_key, &log_path),
    )
    .await?;

    let (adapter_event_tx, adapter_event_rx) = mpsc::channel(ADAPTER_EVENT_CHANNEL_CAPACITY);
    let (adapter_log_line_tx, adapter_log_line_rx) =
        mpsc::channel(ADAPTER_LOG_LINE_CHANNEL_CAPACITY);
    let event_persist_task = tokio::spawn(persist_adapter_events(
        pool.clone(),
        run.id,
        run.dag_run_id,
        log_path.clone(),
        adapter_event_rx,
    ));
    let log_line_persist_task = tokio::spawn(persist_adapter_log_lines(
        pool.clone(),
        run.id,
        run.dag_run_id,
        adapter_log_line_rx,
    ));

    let (adapter_cancel_tx, adapter_cancel_rx) = watch::channel(false);
    let mut response = Box::pin(
        crate::dag_apps::run_app_action_with_idempotency_key_checkpoint_events_and_cancellation(
            &run.app_id,
            &run.action_id,
            run.input.args.clone(),
            run.input.input.clone(),
            run.input.json,
            run.input.dry_run,
            idempotency_key,
            run.input.checkpoint.clone(),
            Some(adapter_event_tx),
            Some(adapter_log_line_tx),
            Some(adapter_cancel_rx),
        ),
    );
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    let mut cancellation_check = tokio::time::interval(Duration::from_secs(5));
    let mut cancellation_requested = false;
    let outcome = loop {
        tokio::select! {
            result = &mut response => {
                if cancellation_requested {
                    break AdapterRunOutcome::Cancelled(result);
                }
                break AdapterRunOutcome::Response(result);
            }
            _ = heartbeat.tick() => {
                if let Err(err) = app_runs::heartbeat_worker(pool, run.worker_id).await {
                    tracing::warn!(
                        target: "agenthero::scheduler",
                        err = %err,
                        event_type = "app_run.heartbeat_failed",
                        app_run_id = %trace.app_run_id,
                        dag_run_id = %trace.dag_run_id,
                        node_id = %trace.node_id,
                        attempt = trace.attempt,
                        node_kind = %trace.node_kind,
                        tool_id = %trace.tool_id,
                        manifest_hash = %trace.manifest_hash,
                        artifact_id = %trace.artifact_id,
                        lease_id = %trace.lease_id,
                        status = %trace.status,
                        exit_status = trace.exit_status,
                        duration_ms = trace.duration_ms,
                        worker_id = %trace.worker_id,
                        app = %trace.app,
                        action = %trace.action,
                        dag_type = %trace.dag_type,
                        "failed to refresh app-run worker heartbeat"
                    );
                }
                if let Err(err) = app_runs::renew_lease(pool, run.lease_id).await {
                    tracing::warn!(
                        target: "agenthero::scheduler",
                        err = %err,
                        event_type = "app_run.lease_renewal_failed",
                        app_run_id = %trace.app_run_id,
                        dag_run_id = %trace.dag_run_id,
                        node_id = %trace.node_id,
                        attempt = trace.attempt,
                        node_kind = %trace.node_kind,
                        tool_id = %trace.tool_id,
                        manifest_hash = %trace.manifest_hash,
                        artifact_id = %trace.artifact_id,
                        lease_id = %trace.lease_id,
                        status = %trace.status,
                        exit_status = trace.exit_status,
                        duration_ms = trace.duration_ms,
                        worker_id = %trace.worker_id,
                        app = %trace.app,
                        action = %trace.action,
                        dag_type = %trace.dag_type,
                        "failed to renew app-run lease"
                    );
                }
            }
            _ = cancellation_check.tick() => {
                if !cancellation_requested && app_runs::run_state(pool, run.id).await?.as_deref() == Some("cancelled") {
                    let _ = adapter_cancel_tx.send(true);
                    cancellation_requested = true;
                    append_app_run_log_event(
                        &log_path,
                        "warn",
                        "app_run.cancel_observed",
                        "worker observed operator cancellation",
                    );
                    app_runs::insert_event(
                        pool,
                        run.id,
                        "warn",
                        "app_run.cancel_observed",
                        Some("worker observed operator cancellation"),
                        app_run_cancellation_event_payload(
                            &trace,
                            json!({
                                "status": "cancelled",
                            }),
                        ),
                    )
                    .await?;
                }
            }
        }
    };
    drop(response);
    let observed_lifecycle =
        join_adapter_persist_tasks(run.id, event_persist_task, log_line_persist_task).await;

    match outcome {
        AdapterRunOutcome::Cancelled(cleanup_result) => {
            app_runs::release_lease(pool, run.lease_id, "cancelled").await?;
            let cancel_message = "worker stopped adapter after operator cancellation";
            app_runs::cancel_dag_run(pool, run.id, run.dag_run_id, cancel_message).await?;
            let cancelled_nodes =
                app_runs::cancel_live_nodes(pool, run.id, run.dag_run_id, cancel_message).await?;
            app_runs::insert_dag_event(
                pool,
                run.id,
                run.dag_run_id,
                "warn",
                "dag.cancelled",
                Some(cancel_message),
                app_run_cancellation_event_payload(
                    &trace,
                    json!({
                        "cancelled_nodes": cancelled_nodes,
                        "status": "cancelled",
                    }),
                ),
            )
            .await?;
            app_runs::insert_dag_event(
                pool,
                run.id,
                run.dag_run_id,
                "warn",
                "app_action.cancelled",
                Some(&format!(
                    "{} action `{}` cancelled",
                    run.app_id, run.action_id
                )),
                app_run_cancellation_event_payload(
                    &trace,
                    json!({
                        "status": "cancelled",
                        "exit_status": null,
                    }),
                ),
            )
            .await?;
            if let Err(err) = cleanup_result.as_ref() {
                if !is_expected_adapter_cancellation(err) {
                    let message = format!("{err:#}");
                    tracing::warn!(
                        target: "agenthero::scheduler",
                        err = %message,
                        event_type = "app_run.cancel_cleanup_error",
                        app_run_id = %trace.app_run_id,
                        dag_run_id = %trace.dag_run_id,
                        node_id = %trace.node_id,
                        attempt = trace.attempt,
                        node_kind = %trace.node_kind,
                        tool_id = %trace.tool_id,
                        manifest_hash = %trace.manifest_hash,
                        artifact_id = %trace.artifact_id,
                        lease_id = %trace.lease_id,
                        status = "cancelled",
                        exit_status = trace.exit_status,
                        duration_ms = trace.duration_ms,
                        worker_id = %trace.worker_id,
                        app = %trace.app,
                        action = %trace.action,
                        dag_type = %trace.dag_type,
                        "adapter cancellation cleanup returned an error"
                    );
                    app_runs::insert_event(
                        pool,
                        run.id,
                        "warn",
                        "app_run.cancel_cleanup_error",
                        Some("adapter cancellation cleanup returned an error"),
                        app_run_cancellation_event_payload(
                            &trace,
                            json!({
                                "status": "cancelled",
                                "error": message,
                            }),
                        ),
                    )
                    .await?;
                    append_app_run_log_event(
                        &log_path,
                        "warn",
                        "app_run.cancel_cleanup_error",
                        "adapter cancellation cleanup returned an error",
                    );
                }
            }
            app_runs::insert_event(
                pool,
                run.id,
                "info",
                "app_run.cancel_cleanup_finished",
                Some("worker stopped adapter and drained observable output after cancellation"),
                app_run_cancellation_event_payload(
                    &trace,
                    json!({
                        "status": "cancelled",
                    }),
                ),
            )
            .await?;
            append_app_run_log_event(
                &log_path,
                "warn",
                "app_action.cancelled",
                &format!("{} action `{}` cancelled", run.app_id, run.action_id),
            );
            append_app_run_log_event(
                &log_path,
                "info",
                "app_run.cancel_cleanup_finished",
                "worker stopped adapter and drained observable output after cancellation",
            );
        }
        AdapterRunOutcome::Response(response) => match response {
            Ok(response) if response.ok => {
                let output = serde_json::to_value(&response)?;
                if let Some(message) = failed_report_message(response.report.as_ref()) {
                    let updated = app_runs::complete_failure_with_runtime_observability(
                        pool,
                        run.id,
                        Some(app_run_runtime_identity(&run)),
                        "failed",
                        "dag_failed",
                        &message,
                        false,
                        Some(output),
                        response.report.as_ref(),
                    )
                    .await?;
                    app_runs::release_lease(pool, run.lease_id, "failed").await?;
                    if updated {
                        if !observed_lifecycle.has("app_action.failed") {
                            app_runs::insert_dag_event(
                                pool,
                                run.id,
                                run.dag_run_id,
                                "error",
                                "app_action.failed",
                                Some(&format!(
                                    "{} action `{}` failed: {message}",
                                    run.app_id, run.action_id
                                )),
                                app_action_failed_event_payload(&run, &dag_type, &message),
                            )
                            .await?;
                            append_app_run_log_event(
                                &log_path,
                                "error",
                                "app_action.failed",
                                &message,
                            );
                        }
                        append_app_run_log_event(&log_path, "error", "app_run.failed", &message);
                    } else {
                        append_app_run_log_event(
                            &log_path,
                            "warn",
                            "app_run.completion_ignored",
                            "app run was no longer running when adapter returned",
                        );
                    }
                } else {
                    let updated = app_runs::complete_success_with_runtime(
                        pool,
                        run.id,
                        Some(app_run_runtime_identity(&run)),
                        output,
                        response.report.as_ref(),
                    )
                    .await?;
                    app_runs::release_lease(pool, run.lease_id, "released").await?;
                    if updated {
                        let (event_type, message) =
                            successful_response_log_event(response.report.as_ref());
                        append_app_run_log_event(&log_path, "info", event_type, message);
                        let (action_event_type, action_message, action_status, action_exit_status) =
                            successful_response_action_event(response.report.as_ref());
                        if !observed_lifecycle.has(action_event_type) {
                            app_runs::insert_dag_event(
                                pool,
                                run.id,
                                run.dag_run_id,
                                "info",
                                action_event_type,
                                Some(&format!(
                                    "{} action `{}` {action_message}",
                                    run.app_id, run.action_id
                                )),
                                app_action_terminal_event_payload(
                                    &run,
                                    &dag_type,
                                    action_status,
                                    action_exit_status,
                                    None,
                                ),
                            )
                            .await?;
                            append_app_run_log_event(
                                &log_path,
                                "info",
                                action_event_type,
                                action_message,
                            );
                        }
                    } else {
                        append_app_run_log_event(
                            &log_path,
                            "warn",
                            "app_run.completion_ignored",
                            "app run was no longer running when adapter returned",
                        );
                    }
                }
            }
            Ok(response) => {
                let message = response
                    .error
                    .clone()
                    .unwrap_or_else(|| "app adapter returned ok=false".to_string());
                let output = serde_json::to_value(&response)?;
                let updated = app_runs::complete_failure_with_runtime_observability(
                    pool,
                    run.id,
                    Some(app_run_runtime_identity(&run)),
                    "failed",
                    "adapter_failed",
                    &message,
                    true,
                    Some(output),
                    response.report.as_ref(),
                )
                .await?;
                app_runs::release_lease(pool, run.lease_id, "failed").await?;
                if updated {
                    if !observed_lifecycle.has("app_action.failed") {
                        app_runs::insert_dag_event(
                            pool,
                            run.id,
                            run.dag_run_id,
                            "error",
                            "app_action.failed",
                            Some(&format!(
                                "{} action `{}` failed: {message}",
                                run.app_id, run.action_id
                            )),
                            app_action_terminal_event_payload(
                                &run,
                                &dag_type,
                                "failed",
                                Some(1),
                                Some(&message),
                            ),
                        )
                        .await?;
                        append_app_run_log_event(&log_path, "error", "app_action.failed", &message);
                    }
                    append_app_run_log_event(&log_path, "error", "app_run.failed", &message);
                } else {
                    append_app_run_log_event(
                        &log_path,
                        "warn",
                        "app_run.completion_ignored",
                        "app run was no longer running when adapter returned",
                    );
                }
            }
            Err(err) => {
                let message = format!("{err:#}");
                let updated = app_runs::complete_failure_with_runtime_observability(
                    pool,
                    run.id,
                    Some(app_run_runtime_identity(&run)),
                    "system_failed",
                    "adapter_system_failed",
                    &message,
                    true,
                    None,
                    None,
                )
                .await?;
                app_runs::release_lease(pool, run.lease_id, "failed").await?;
                if updated {
                    if !observed_lifecycle.has("app_action.failed") {
                        app_runs::insert_dag_event(
                            pool,
                            run.id,
                            run.dag_run_id,
                            "error",
                            "app_action.failed",
                            Some(&format!(
                                "{} action `{}` failed: {message}",
                                run.app_id, run.action_id
                            )),
                            app_action_terminal_event_payload(
                                &run,
                                &dag_type,
                                "system_failed",
                                Some(1),
                                Some(&message),
                            ),
                        )
                        .await?;
                        append_app_run_log_event(&log_path, "error", "app_action.failed", &message);
                    }
                    append_app_run_log_event(&log_path, "error", "app_run.failed", &message);
                } else {
                    append_app_run_log_event(
                        &log_path,
                        "warn",
                        "app_run.completion_ignored",
                        "app run was no longer running when adapter returned",
                    );
                }
            }
        },
    }
    Ok(())
}

fn app_run_runtime_identity(run: &ClaimedAppRun) -> app_runs::AppRunRuntimeIdentity {
    app_runs::AppRunRuntimeIdentity {
        dag_run_id: run.dag_run_id,
        lease_id: run.lease_id,
    }
}

fn failed_report_message(
    report: Option<&agenthero_dag_executor::DagExecutionReport>,
) -> Option<String> {
    let report = report?;
    if report.status != DagNodeStatus::Failed {
        return None;
    }
    if let Some(node) = report
        .nodes
        .iter()
        .find(|node| node.status == DagNodeStatus::Failed)
    {
        if let Some(error) = node.error.as_deref().filter(|error| !error.is_empty()) {
            return Some(error.to_string());
        }
        return Some(format!(
            "DAG `{}` failed at node `{}`",
            report.dag_type, node.node_id
        ));
    }
    if let Some(event) = report
        .events
        .iter()
        .find(|event| event.level == "error")
        .and_then(|event| event.message.as_deref())
        .filter(|message| !message.is_empty())
    {
        return Some(event.to_string());
    }
    Some(format!("DAG `{}` failed", report.dag_type))
}

fn app_action_failed_event_payload(
    run: &ClaimedAppRun,
    dag_type: &str,
    message: &str,
) -> serde_json::Value {
    app_action_terminal_event_payload(run, dag_type, "failed", Some(1), Some(message))
}

fn app_action_terminal_event_payload(
    run: &ClaimedAppRun,
    dag_type: &str,
    status: &str,
    exit_status: Option<i32>,
    error: Option<&str>,
) -> serde_json::Value {
    agenthero_agent_runtime::agenthero_trace_payload(
        run.id,
        None,
        json!({
            "app": run.app_id,
            "action": run.action_id,
            "dag_type": dag_type,
            "dag_run_id": run.dag_run_id.to_string(),
            "lease_id": run.lease_id.to_string(),
            "attempt": run.attempt,
            "status": status,
            "exit_status": exit_status,
            "error": error,
        }),
    )
}

fn is_expected_adapter_cancellation(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("adapter cancelled by AgentHero")
}

async fn join_adapter_persist_tasks(
    run_id: Uuid,
    event_persist_task: JoinHandle<AdapterObservedLifecycle>,
    log_line_persist_task: JoinHandle<()>,
) -> AdapterObservedLifecycle {
    let observed = join_adapter_event_persist_task(
        run_id,
        event_persist_task,
        "app_run.adapter_event_persister_join_failed",
        "app_run.adapter_event_persister_join_timeout",
        "adapter event persister",
    )
    .await;
    join_one_adapter_persist_task(
        run_id,
        log_line_persist_task,
        "app_run.adapter_log_persister_join_failed",
        "app_run.adapter_log_persister_join_timeout",
        "adapter log-line persister",
    )
    .await;
    observed
}

async fn join_adapter_event_persist_task(
    run_id: Uuid,
    mut task: JoinHandle<AdapterObservedLifecycle>,
    failed_event_type: &'static str,
    timeout_event_type: &'static str,
    label: &'static str,
) -> AdapterObservedLifecycle {
    let result = tokio::select! {
        result = &mut task => result,
        _ = tokio::time::sleep(ADAPTER_PERSIST_JOIN_TIMEOUT) => {
            task.abort();
            let _ = task.await;
            tracing::warn!(
                target: "agenthero::scheduler",
                event_type = timeout_event_type,
                app_run_id = %run_id,
                persister = label,
                dag_run_id = "",
                node_id = "",
                attempt = 0_i32,
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "system_failed",
                exit_status = 0_i64,
                duration_ms = ADAPTER_PERSIST_JOIN_TIMEOUT.as_millis() as u64,
                "aborted stuck adapter persister during cleanup"
            );
            return AdapterObservedLifecycle::default();
        }
    };

    match result {
        Ok(observed) => observed,
        Err(err) => {
            tracing::warn!(
                target: "agenthero::scheduler",
                err = %err,
                event_type = failed_event_type,
                app_run_id = %run_id,
                persister = label,
                dag_run_id = "",
                node_id = "",
                attempt = 0_i32,
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "system_failed",
                exit_status = 0_i64,
                duration_ms = 0_u64,
                "failed to join adapter persister"
            );
            AdapterObservedLifecycle::default()
        }
    }
}

async fn join_one_adapter_persist_task(
    run_id: Uuid,
    mut task: JoinHandle<()>,
    failed_event_type: &'static str,
    timeout_event_type: &'static str,
    label: &'static str,
) {
    let result = tokio::select! {
        result = &mut task => result,
        _ = tokio::time::sleep(ADAPTER_PERSIST_JOIN_TIMEOUT) => {
            task.abort();
            let _ = task.await;
            tracing::warn!(
                target: "agenthero::scheduler",
                event_type = timeout_event_type,
                app_run_id = %run_id,
                persister = label,
                dag_run_id = "",
                node_id = "",
                attempt = 0_i32,
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "system_failed",
                exit_status = 0_i64,
                duration_ms = ADAPTER_PERSIST_JOIN_TIMEOUT.as_millis() as u64,
                "aborted stuck adapter persister during cleanup"
            );
            return;
        }
    };

    if let Err(err) = result {
        tracing::warn!(
            target: "agenthero::scheduler",
            err = %err,
            event_type = failed_event_type,
            app_run_id = %run_id,
            persister = label,
            dag_run_id = "",
            node_id = "",
            attempt = 0_i32,
            node_kind = "",
            tool_id = "",
            manifest_hash = "",
            artifact_id = "",
            lease_id = "",
            status = "system_failed",
            exit_status = 0_i64,
            duration_ms = 0_u64,
            "failed to join adapter persister"
        );
    }
}

async fn persist_adapter_events(
    pool: PgPool,
    run_id: Uuid,
    dag_run_id: Uuid,
    log_path: std::path::PathBuf,
    mut events: mpsc::Receiver<agenthero_dag_executor::DagExecutionEvent>,
) -> AdapterObservedLifecycle {
    let mut budget = AdapterEventPersistBudget::default();
    let mut observed = AdapterObservedLifecycle::default();
    while let Some(event) = events.recv().await {
        let Ok(serialized_event) = serde_json::to_vec(&event) else {
            tracing::warn!(
                target: "agenthero::scheduler",
                app_run_id = %run_id,
                dag_run_id = %dag_run_id,
                node_id = %event.node_id.as_deref().unwrap_or_default(),
                attempt = event
                    .payload
                    .get("attempt")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default(),
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "failed",
                exit_status = 0_i64,
                duration_ms = 0_u64,
                event_type = %event.event_type,
                "failed to serialize live adapter event for budget accounting"
            );
            continue;
        };
        let Some(()) = budget.take(serialized_event.len()) else {
            if !budget.truncation_recorded {
                budget.truncation_recorded = true;
                let message = "adapter event stream truncated by AgentHero budget";
                append_app_run_log_event(
                    &log_path,
                    "warn",
                    "app_run.adapter_events_truncated",
                    message,
                );
                if let Err(err) = app_runs::insert_dag_event(
                    &pool,
                    run_id,
                    dag_run_id,
                    "warn",
                    "app_run.adapter_events_truncated",
                    Some(message),
                    json!({
                        "app_run_id": run_id.to_string(),
                        "source": "adapter_event_stream",
                        "max_rows": MAX_ADAPTER_EVENT_ROWS,
                        "max_bytes": MAX_ADAPTER_EVENT_BYTES,
                    }),
                )
                .await
                {
                    tracing::warn!(
                        target: "agenthero::scheduler",
                        err = %err,
                        event_type = "app_run.adapter_events_truncated_persist_failed",
                        app_run_id = %run_id,
                        dag_run_id = %dag_run_id,
                        node_id = "",
                        attempt = 0_i32,
                        node_kind = "",
                        tool_id = "",
                        manifest_hash = "",
                        artifact_id = "",
                        lease_id = "",
                        status = "failed",
                        exit_status = 0_i64,
                        duration_ms = 0_u64,
                        "failed to persist adapter event truncation notice"
                    );
                }
            }
            continue;
        };
        append_adapter_event_log(&log_path, run_id, dag_run_id, &event);
        match app_runs::insert_dag_event(
            &pool,
            run_id,
            dag_run_id,
            &event.level,
            &event.event_type,
            event.message.as_deref(),
            adapter_event_payload(run_id, dag_run_id, &event),
        )
        .await
        {
            Ok(()) => observed.record_persisted(&event.event_type),
            Err(err) => {
                tracing::warn!(
                    target: "agenthero::scheduler",
                    err = %err,
                    app_run_id = %run_id,
                    dag_run_id = %dag_run_id,
                    node_id = %event.node_id.as_deref().unwrap_or_default(),
                    attempt = event
                        .payload
                        .get("attempt")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default(),
                    node_kind = event
                        .payload
                        .get("node_kind")
                        .or_else(|| event.payload.get("kind"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    tool_id = event
                        .payload
                        .get("tool_id")
                        .or_else(|| event.payload.get("tool"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    manifest_hash = event
                        .payload
                        .get("manifest_hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    artifact_id = event
                        .payload
                        .get("artifact_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    lease_id = event
                        .payload
                        .get("lease_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    status = event
                        .payload
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("failed"),
                    exit_status = event
                        .payload
                        .get("exit_status")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default(),
                    duration_ms = event
                        .payload
                        .get("duration_ms")
                        .or_else(|| event.payload.get("latency_ms"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                    event_type = %event.event_type,
                    "failed to persist live adapter event"
                );
            }
        }
    }
    observed
}

async fn persist_adapter_log_lines(
    pool: PgPool,
    run_id: Uuid,
    dag_run_id: Uuid,
    mut lines: mpsc::Receiver<String>,
) {
    let mut budget = AdapterLogPersistBudget::default();
    while let Some(line) = lines.recv().await {
        let Some(line) = budget.take(&line) else {
            continue;
        };
        let (message, truncated) = bounded_log_line(&line);
        if let Err(err) = app_runs::insert_dag_event(
            &pool,
            run_id,
            dag_run_id,
            "info",
            "app_log.stderr",
            Some(&message),
            adapter_stderr_payload(run_id, dag_run_id, truncated),
        )
        .await
        {
            tracing::warn!(
                target: "agenthero::scheduler",
                err = %err,
                event_type = "app_log.stderr_persist_failed",
                app_run_id = %run_id,
                dag_run_id = %dag_run_id,
                node_id = "",
                attempt = 0_i32,
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "failed",
                exit_status = 0_i64,
                duration_ms = 0_u64,
                "failed to persist adapter stderr log line"
            );
        }
    }
}

struct AdapterEventPersistBudget {
    rows: usize,
    bytes: usize,
    truncation_recorded: bool,
}

impl Default for AdapterEventPersistBudget {
    fn default() -> Self {
        Self {
            rows: MAX_ADAPTER_EVENT_ROWS,
            bytes: MAX_ADAPTER_EVENT_BYTES,
            truncation_recorded: false,
        }
    }
}

impl AdapterEventPersistBudget {
    fn take(&mut self, len: usize) -> Option<()> {
        if self.rows == 0 || self.bytes == 0 {
            return None;
        }
        let charged_len = len.max(1);
        if charged_len > self.bytes {
            self.rows = self.rows.saturating_sub(1);
            self.bytes = 0;
            return None;
        }
        self.rows = self.rows.saturating_sub(1);
        self.bytes = self.bytes.saturating_sub(charged_len);
        Some(())
    }
}

struct AdapterLogPersistBudget {
    rows: usize,
    bytes: usize,
}

impl Default for AdapterLogPersistBudget {
    fn default() -> Self {
        Self { rows: 0, bytes: 0 }
    }
}

impl AdapterLogPersistBudget {
    fn take(&mut self, line: &str) -> Option<String> {
        if self.rows >= MAX_ADAPTER_LOG_EVENT_ROWS || self.bytes >= MAX_ADAPTER_LOG_EVENT_BYTES {
            return None;
        }
        let remaining = MAX_ADAPTER_LOG_EVENT_BYTES.saturating_sub(self.bytes);
        let taken = take_prefix_bytes(line, remaining)?;
        self.rows += 1;
        self.bytes = self.bytes.saturating_add(taken.len());
        Some(taken)
    }
}

fn take_prefix_bytes(value: &str, limit: usize) -> Option<String> {
    if limit == 0 {
        return None;
    }
    if value.len() <= limit {
        return Some(value.to_string());
    }
    let mut end = 0;
    for (index, ch) in value.char_indices() {
        let next = index + ch.len_utf8();
        if next > limit {
            break;
        }
        end = next;
    }
    (end > 0).then(|| value[..end].to_string())
}

fn bounded_log_line(line: &str) -> (String, bool) {
    const MAX_LOG_LINE_CHARS: usize = 4096;
    if line.chars().count() <= MAX_LOG_LINE_CHARS {
        return (line.to_string(), false);
    }
    (
        format!(
            "{}...<truncated>",
            line.chars().take(MAX_LOG_LINE_CHARS).collect::<String>()
        ),
        true,
    )
}

fn append_adapter_event_log(
    path: &Path,
    run_id: Uuid,
    dag_run_id: Uuid,
    event: &agenthero_dag_executor::DagExecutionEvent,
) {
    let event = adapter_event_for_log(run_id, dag_run_id, event);
    append_app_run_log_event(
        path,
        &event.level,
        &event.event_type,
        &adapter_event_log_message(&event),
    );
    append_structured_adapter_event_log(path, &event);
}

fn adapter_event_for_log(
    run_id: Uuid,
    dag_run_id: Uuid,
    event: &agenthero_dag_executor::DagExecutionEvent,
) -> agenthero_dag_executor::DagExecutionEvent {
    let payload = serde_json::from_value(adapter_event_payload(run_id, dag_run_id, event))
        .unwrap_or_else(|_| event.payload.clone());
    agenthero_dag_executor::DagExecutionEvent {
        payload,
        ..event.clone()
    }
}

fn adapter_event_payload(
    run_id: Uuid,
    dag_run_id: Uuid,
    event: &agenthero_dag_executor::DagExecutionEvent,
) -> serde_json::Value {
    let mut payload = agenthero_agent_runtime::agenthero_event_payload(run_id, event);
    if let Some(object) = payload.as_object_mut() {
        object.insert("dag_run_id".to_string(), json!(dag_run_id.to_string()));
    }
    payload
}

fn adapter_stderr_payload(run_id: Uuid, dag_run_id: Uuid, truncated: bool) -> serde_json::Value {
    agenthero_agent_runtime::agenthero_trace_payload(
        run_id,
        None,
        json!({
            "dag_run_id": dag_run_id.to_string(),
            "source": "adapter_stderr",
            "truncated": truncated,
        }),
    )
}

fn app_run_started_event_payload(
    run: &ClaimedAppRun,
    dag_type: &str,
    idempotency_key: &str,
    log_path: &Path,
) -> serde_json::Value {
    agenthero_agent_runtime::agenthero_trace_payload(
        run.id,
        None,
        json!({
            "app": run.app_id,
            "action": run.action_id,
            "dag_type": dag_type,
            "attempt": run.attempt,
            "dag_run_id": run.dag_run_id.to_string(),
            "lease_id": run.lease_id.to_string(),
            "worker_id": run.worker_id.to_string(),
            "status": "running",
            "idempotency_key": idempotency_key,
            "log_path": log_path.to_string_lossy(),
            "retry": { "max_attempts": run.input.retry.max_attempts },
        }),
    )
}

fn app_run_cancellation_event_payload(
    trace: &SchedulerTraceContext,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut payload =
        agenthero_agent_runtime::agenthero_trace_payload(&trace.app_run_id, None, payload);
    if let Some(object) = payload.as_object_mut() {
        object.insert("dag_run_id".to_string(), json!(trace.dag_run_id));
        object.insert("lease_id".to_string(), json!(trace.lease_id));
        object.insert("attempt".to_string(), json!(trace.attempt));
        object.insert("worker_id".to_string(), json!(trace.worker_id));
        object.insert("app".to_string(), json!(trace.app));
        object.insert("action".to_string(), json!(trace.action));
        object.insert("dag_type".to_string(), json!(trace.dag_type));
    }
    payload
}

fn adapter_event_log_message(event: &agenthero_dag_executor::DagExecutionEvent) -> String {
    let mut message = event
        .message
        .clone()
        .unwrap_or_else(|| event.event_type.clone());
    if let Some(node_id) = &event.node_id {
        message.push_str(&format!(" node={node_id}"));
    }
    if let Some(attempt) = event
        .payload
        .get("attempt")
        .and_then(serde_json::Value::as_i64)
    {
        message.push_str(&format!(" attempt={attempt}"));
    }
    for field in ["next_attempt", "max_attempts", "backoff_ms"] {
        if let Some(value) = event.payload.get(field).and_then(serde_json::Value::as_i64) {
            message.push_str(&format!(" {field}={value}"));
        }
    }
    if let Some(status) = event
        .payload
        .get("status")
        .and_then(serde_json::Value::as_str)
    {
        message.push_str(&format!(" status={status}"));
    }
    message
}

fn successful_response_log_event(
    report: Option<&agenthero_dag_executor::DagExecutionReport>,
) -> (&'static str, &'static str) {
    if report.is_some_and(|report| report.status == DagNodeStatus::AwaitingApproval) {
        return ("app_run.awaiting_approval", "app run awaiting approval");
    }
    ("app_run.finished", "app run finished")
}

fn successful_response_action_event(
    report: Option<&agenthero_dag_executor::DagExecutionReport>,
) -> (&'static str, &'static str, &'static str, Option<i32>) {
    match report.map(|report| report.status) {
        Some(DagNodeStatus::AwaitingApproval) => (
            "app_action.awaiting_approval",
            "awaiting approval",
            "awaiting_approval",
            None,
        ),
        Some(DagNodeStatus::Degraded | DagNodeStatus::Skipped) => (
            "app_action.completed",
            "completed with partial status",
            "partial",
            Some(0),
        ),
        _ => ("app_action.completed", "completed", "ok", Some(0)),
    }
}

fn append_app_run_log_event(path: &Path, level: &str, event: &str, message: &str) {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let line = format!("{timestamp} {level:<5} {event:<28} {message}");
    if let Err(err) = append_app_run_log_line(path, &line) {
        tracing::warn!(err = %err, path = %path.display(), "failed to append app-run log line");
    }
}

fn append_structured_adapter_event_log(
    path: &Path,
    event: &agenthero_dag_executor::DagExecutionEvent,
) {
    let mut bytes = Vec::new();
    match agenthero_agent_runtime::write_adapter_event(&mut bytes, event) {
        Ok(()) => {
            let line = String::from_utf8_lossy(&bytes);
            let line = line.trim_end_matches(|ch| ch == '\r' || ch == '\n');
            if let Err(err) = append_app_run_log_line(path, line) {
                tracing::warn!(
                    err = %err,
                    path = %path.display(),
                    "failed to append structured app-run log event"
                );
            }
        }
        Err(err) => {
            tracing::warn!(
                err = %err,
                path = %path.display(),
                "failed to serialize structured app-run log event"
            );
        }
    }
}

fn append_app_run_log_line(path: &Path, line: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn prepare_app_run_worker_input(
    input: &mut agenthero_dag_executor::DagIo,
    run_id: Uuid,
    dag_run_id: Uuid,
    lease_id: Uuid,
    stream_stderr: bool,
    debug_logs: bool,
) {
    input
        .values
        .insert("app_run_id".to_string(), json!(run_id.to_string()));
    input
        .values
        .insert("dag_run_id".to_string(), json!(dag_run_id.to_string()));
    input
        .values
        .insert("lease_id".to_string(), json!(lease_id.to_string()));
    input.values.insert(
        crate::dag_apps::APP_RUN_LOG_PATH_INPUT_KEY.to_string(),
        json!(crate::dag_apps::app_run_log_path(run_id).to_string_lossy()),
    );
    if stream_stderr {
        input
            .values
            .insert("stream_stderr".to_string(), json!(true));
    }
    if debug_logs {
        input.values.insert("debug_logs".to_string(), json!(true));
    }
}

fn app_run_idempotency_key(run_id: Uuid) -> String {
    format!("app-run:{run_id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchedulerTraceContext {
    app_run_id: String,
    dag_run_id: String,
    node_id: &'static str,
    attempt: i32,
    node_kind: &'static str,
    tool_id: &'static str,
    manifest_hash: &'static str,
    artifact_id: &'static str,
    lease_id: String,
    status: &'static str,
    exit_status: i64,
    duration_ms: u64,
    worker_id: String,
    app: String,
    action: String,
    dag_type: String,
}

impl SchedulerTraceContext {
    fn from_claimed_run(run: &ClaimedAppRun, dag_type: &str) -> Self {
        Self {
            app_run_id: run.id.to_string(),
            dag_run_id: run.dag_run_id.to_string(),
            node_id: "",
            attempt: run.attempt,
            node_kind: "",
            tool_id: "",
            manifest_hash: "",
            artifact_id: "",
            lease_id: run.lease_id.to_string(),
            status: "running",
            exit_status: 0,
            duration_ms: 0,
            worker_id: run.worker_id.to_string(),
            app: run.app_id.clone(),
            action: run.action_id.clone(),
            dag_type: dag_type.to_string(),
        }
    }

    #[cfg(test)]
    fn field_value(&self, field: &str) -> Option<String> {
        match field {
            "app_run_id" => Some(self.app_run_id.clone()),
            "dag_run_id" => Some(self.dag_run_id.clone()),
            "node_id" => Some(self.node_id.to_string()),
            "attempt" => Some(self.attempt.to_string()),
            "node_kind" => Some(self.node_kind.to_string()),
            "tool_id" => Some(self.tool_id.to_string()),
            "manifest_hash" => Some(self.manifest_hash.to_string()),
            "artifact_id" => Some(self.artifact_id.to_string()),
            "lease_id" => Some(self.lease_id.clone()),
            "status" => Some(self.status.to_string()),
            "exit_status" => Some(self.exit_status.to_string()),
            "duration_ms" => Some(self.duration_ms.to_string()),
            _ => None,
        }
    }
}

fn claimed_run_dag_type(run: &ClaimedAppRun) -> anyhow::Result<String> {
    if let Some(dag_type) = app_runs::dag_type_for_claimed_run(&run.input) {
        return Ok(dag_type.to_string());
    }
    Ok(crate::dag_apps::app_action_binding(&run.app_id, &run.action_id)?.dag_type)
}

fn idle_sleep_duration(base: Duration, idle_polls: u32) -> Duration {
    let multiplier = idle_polls.clamp(1, 15);
    base.saturating_mul(multiplier).min(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenthero_dag_executor::{DagExecutionEvent, DagExecutionReport, DagIo};
    use agenthero_dag_runtime::{DagNodeReport, DagTypeId};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    fn sample_claimed_run() -> ClaimedAppRun {
        ClaimedAppRun {
            id: Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap(),
            worker_id: Uuid::parse_str("c762228f-f0be-4fef-a8b5-6993c4f9704f").unwrap(),
            app_id: "sample-app".to_string(),
            action_id: "run".to_string(),
            input: crate::app_runs::StoredAppRunInput::default(),
            dag_run_id: Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap(),
            lease_id: Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap(),
            attempt: 2,
        }
    }

    #[test]
    fn failed_report_message_prefers_failed_node_error() {
        let report = DagExecutionReport {
            dag_type: DagTypeId::new("policy-denial-smoke"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::Failed,
            input: DagIo::default(),
            nodes: vec![DagNodeReport {
                node_id: "network_denied_shell".to_string(),
                kind: "tool".to_string(),
                status: DagNodeStatus::Failed,
                attempt: 1,
                role: None,
                tool: Some("network_denied_shell".to_string()),
                child_dag_type: None,
                required: true,
                executor: Some("shell".to_string()),
                model: None,
                prompt_hash: None,
                command: None,
                exit_status: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                input_refs: Default::default(),
                output_refs: Default::default(),
                diagnostic_refs: Default::default(),
                policy: Default::default(),
                warning: None,
                error: Some("network policy denied before spawning".to_string()),
                latency_ms: Some(1),
                trace: Default::default(),
            }],
            outputs: DagIo::default(),
            events: vec![],
        };

        assert_eq!(
            failed_report_message(Some(&report)).as_deref(),
            Some("network policy denied before spawning")
        );
    }

    #[test]
    fn failed_report_message_falls_back_to_failed_node_identity() {
        let report = DagExecutionReport {
            dag_type: DagTypeId::new("policy-denial-smoke"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::Failed,
            input: DagIo::default(),
            nodes: vec![DagNodeReport {
                node_id: "network_denied_shell".to_string(),
                kind: "tool".to_string(),
                status: DagNodeStatus::Failed,
                attempt: 1,
                role: None,
                tool: Some("network_denied_shell".to_string()),
                child_dag_type: None,
                required: true,
                executor: Some("shell".to_string()),
                model: None,
                prompt_hash: None,
                command: None,
                exit_status: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                input_refs: Default::default(),
                output_refs: Default::default(),
                diagnostic_refs: Default::default(),
                policy: Default::default(),
                warning: None,
                error: None,
                latency_ms: Some(1),
                trace: Default::default(),
            }],
            outputs: DagIo::default(),
            events: vec![DagExecutionEvent {
                level: "error".to_string(),
                event_type: "dag.failed".to_string(),
                node_id: None,
                message: Some("generic event failure".to_string()),
                payload: Default::default(),
            }],
        };

        assert_eq!(
            failed_report_message(Some(&report)).as_deref(),
            Some("DAG `policy-denial-smoke` failed at node `network_denied_shell`")
        );
    }

    #[test]
    fn failed_report_message_ignores_non_failed_report() {
        let report = DagExecutionReport {
            dag_type: DagTypeId::new("tool-policy-smoke"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::Ok,
            input: DagIo::default(),
            nodes: Vec::new(),
            outputs: DagIo::default(),
            events: Vec::new(),
        };

        assert_eq!(failed_report_message(Some(&report)), None);
    }

    #[test]
    fn successful_response_log_event_marks_awaiting_approval_without_finished() {
        let report = DagExecutionReport {
            dag_type: DagTypeId::new("approval-pause-smoke"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::AwaitingApproval,
            input: DagIo::default(),
            nodes: Vec::new(),
            outputs: DagIo::default(),
            events: Vec::new(),
        };

        assert_eq!(
            successful_response_log_event(Some(&report)),
            ("app_run.awaiting_approval", "app run awaiting approval")
        );
    }

    #[test]
    fn successful_response_action_event_marks_completion_approval_and_partial_status() {
        let ok = DagExecutionReport {
            dag_type: DagTypeId::new("tool-policy-smoke"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::Ok,
            input: DagIo::default(),
            nodes: Vec::new(),
            outputs: DagIo::default(),
            events: Vec::new(),
        };
        let partial = DagExecutionReport {
            status: DagNodeStatus::Degraded,
            ..ok.clone()
        };
        let approval = DagExecutionReport {
            status: DagNodeStatus::AwaitingApproval,
            ..ok.clone()
        };

        assert_eq!(
            successful_response_action_event(Some(&ok)),
            ("app_action.completed", "completed", "ok", Some(0))
        );
        assert_eq!(
            successful_response_action_event(Some(&partial)),
            (
                "app_action.completed",
                "completed with partial status",
                "partial",
                Some(0)
            )
        );
        assert_eq!(
            successful_response_action_event(Some(&approval)),
            (
                "app_action.awaiting_approval",
                "awaiting approval",
                "awaiting_approval",
                None
            )
        );
    }

    #[test]
    fn app_action_terminal_payload_includes_monitor_identity_and_trace_fields() {
        let run = sample_claimed_run();

        let payload = app_action_terminal_event_payload(&run, "sample-dag", "ok", Some(0), None);

        assert_eq!(payload["app_run_id"], json!(run.id.to_string()));
        assert_eq!(payload["dag_run_id"], json!(run.dag_run_id.to_string()));
        assert_eq!(payload["lease_id"], json!(run.lease_id.to_string()));
        assert_eq!(payload["app"], json!("sample-app"));
        assert_eq!(payload["action"], json!("run"));
        assert_eq!(payload["dag_type"], json!("sample-dag"));
        assert_eq!(payload["attempt"], json!(2));
        assert_eq!(payload["status"], json!("ok"));
        assert_eq!(payload["exit_status"], json!(0));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                payload.get(*field).is_some(),
                "app action payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn app_action_failed_payload_records_error_and_failed_status() {
        let run = sample_claimed_run();

        let payload = app_action_failed_event_payload(&run, "sample-dag", "adapter failed");

        assert_eq!(payload["app_run_id"], json!(run.id.to_string()));
        assert_eq!(payload["dag_run_id"], json!(run.dag_run_id.to_string()));
        assert_eq!(payload["status"], json!("failed"));
        assert_eq!(payload["exit_status"], json!(1));
        assert_eq!(payload["error"], json!("adapter failed"));
    }

    #[test]
    fn observed_lifecycle_tracks_persisted_app_action_events() {
        let mut observed = AdapterObservedLifecycle::default();

        observed.record_persisted("app_action.started");
        observed.record_persisted("app_action.completed");
        observed.record_persisted("node.completed");

        assert!(observed.has("app_action.started"));
        assert!(observed.has("app_action.completed"));
        assert!(!observed.has("app_action.failed"));
        assert!(!observed.has("node.completed"));
    }

    #[test]
    fn scheduler_idempotency_key_is_stable_for_app_run() {
        let run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        assert_eq!(
            app_run_idempotency_key(run_id),
            "app-run:11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn idle_sleep_duration_backs_off_and_caps() {
        let base = Duration::from_secs(2);

        assert_eq!(idle_sleep_duration(base, 1), Duration::from_secs(2));
        assert_eq!(idle_sleep_duration(base, 3), Duration::from_secs(6));
        assert_eq!(idle_sleep_duration(base, 99), Duration::from_secs(30));
    }

    #[test]
    fn app_run_worker_input_always_carries_durable_identity_and_log_path() {
        let run_id = Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let dag_run_id_text = dag_run_id.to_string();
        let lease_id_text = lease_id.to_string();
        let mut input = agenthero_dag_executor::DagIo::default();

        prepare_app_run_worker_input(&mut input, run_id, dag_run_id, lease_id, false, false);

        assert_eq!(
            input
                .values
                .get("app_run_id")
                .and_then(|value| value.as_str()),
            Some("2d0a1d88-b9f9-4e8f-848e-605b86717330")
        );
        assert_eq!(
            input
                .values
                .get("dag_run_id")
                .and_then(|value| value.as_str()),
            Some(dag_run_id_text.as_str())
        );
        assert_eq!(
            input
                .values
                .get("lease_id")
                .and_then(|value| value.as_str()),
            Some(lease_id_text.as_str())
        );
        assert_eq!(
            input
                .values
                .get(crate::dag_apps::APP_RUN_LOG_PATH_INPUT_KEY)
                .and_then(|value| value.as_str()),
            Some(
                crate::dag_apps::app_run_log_path(run_id)
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[test]
    fn app_run_started_payload_includes_lease_worker_and_attempt_identity() {
        let run = ClaimedAppRun {
            id: Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap(),
            worker_id: Uuid::parse_str("c762228f-f0be-4fef-a8b5-6993c4f9704f").unwrap(),
            app_id: "sample-app".to_string(),
            action_id: "run".to_string(),
            input: crate::app_runs::StoredAppRunInput::default(),
            dag_run_id: Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap(),
            lease_id: Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap(),
            attempt: 2,
        };
        let log_path = Path::new(".agenthero/app_runs/2d0a1d88-b9f9-4e8f-848e-605b86717330.log");

        let payload = app_run_started_event_payload(&run, "sample-dag", "app-run:stable", log_path);

        assert_eq!(payload["app_run_id"], run.id.to_string());
        assert_eq!(payload["dag_run_id"], run.dag_run_id.to_string());
        assert_eq!(payload["lease_id"], run.lease_id.to_string());
        assert_eq!(payload["worker_id"], run.worker_id.to_string());
        assert_eq!(payload["attempt"], json!(2));
        assert_eq!(payload["app"], "sample-app");
        assert_eq!(payload["action"], "run");
        assert_eq!(payload["dag_type"], "sample-dag");
        assert_eq!(payload["status"], "running");
    }

    #[test]
    fn app_run_cancellation_payload_includes_dag_and_lease_identity() {
        let run = ClaimedAppRun {
            id: Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap(),
            worker_id: Uuid::parse_str("c762228f-f0be-4fef-a8b5-6993c4f9704f").unwrap(),
            app_id: "sample-app".to_string(),
            action_id: "run".to_string(),
            input: crate::app_runs::StoredAppRunInput::default(),
            dag_run_id: Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap(),
            lease_id: Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap(),
            attempt: 2,
        };
        let trace = SchedulerTraceContext::from_claimed_run(&run, "sample-dag");

        let payload = app_run_cancellation_event_payload(&trace, json!({"status": "cancelled"}));

        assert_eq!(payload["app_run_id"], run.id.to_string());
        assert_eq!(payload["dag_run_id"], run.dag_run_id.to_string());
        assert_eq!(payload["lease_id"], run.lease_id.to_string());
        assert_eq!(payload["attempt"], json!(2));
        assert_eq!(payload["status"], "cancelled");
    }

    #[test]
    fn expected_adapter_cancellation_error_is_not_cleanup_failure() {
        let expected = anyhow::anyhow!("app `platform-smoke` adapter cancelled by AgentHero");
        let unexpected = anyhow::anyhow!("adapter stdout pipe failed");

        assert!(is_expected_adapter_cancellation(&expected));
        assert!(!is_expected_adapter_cancellation(&unexpected));
    }

    #[test]
    fn scheduler_trace_context_exposes_agenthero_identity_fields() {
        let run = ClaimedAppRun {
            id: Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap(),
            worker_id: Uuid::parse_str("c762228f-f0be-4fef-a8b5-6993c4f9704f").unwrap(),
            app_id: "sample-app".to_string(),
            action_id: "run".to_string(),
            input: crate::app_runs::StoredAppRunInput::default(),
            dag_run_id: Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap(),
            lease_id: Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap(),
            attempt: 2,
        };

        let context = SchedulerTraceContext::from_claimed_run(&run, "sample-dag");

        assert_eq!(context.app_run_id, run.id.to_string());
        assert_eq!(context.dag_run_id, run.dag_run_id.to_string());
        assert_eq!(context.lease_id, run.lease_id.to_string());
        assert_eq!(context.worker_id, run.worker_id.to_string());
        assert_eq!(context.app, "sample-app");
        assert_eq!(context.action, "run");
        assert_eq!(context.dag_type, "sample-dag");
        assert_eq!(context.attempt, 2);
        assert_eq!(context.status, "running");
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                context.field_value(field).is_some(),
                "scheduler trace context should expose mandatory AgentHero field `{field}`"
            );
        }
    }

    #[test]
    fn adapter_event_log_message_includes_node_attempt_and_status() {
        let event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("compile".to_string()),
            message: Some("compile Ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("attempt".to_string(), json!(2)),
                ("status".to_string(), json!("ok")),
            ]),
        };

        let message = adapter_event_log_message(&event);

        assert_eq!(message, "compile Ok node=compile attempt=2 status=ok");
    }

    #[test]
    fn adapter_event_log_message_includes_retry_schedule() {
        let event = agenthero_dag_executor::DagExecutionEvent {
            level: "warn".to_string(),
            event_type: "node.retry_scheduled".to_string(),
            node_id: Some("lean_check".to_string()),
            message: Some("lean_check retry scheduled".to_string()),
            payload: std::collections::BTreeMap::from([
                ("attempt".to_string(), json!(1)),
                ("next_attempt".to_string(), json!(2)),
                ("max_attempts".to_string(), json!(3)),
                ("backoff_ms".to_string(), json!(250)),
            ]),
        };

        let message = adapter_event_log_message(&event);

        assert_eq!(
            message,
            "lean_check retry scheduled node=lean_check attempt=1 next_attempt=2 max_attempts=3 backoff_ms=250"
        );
    }

    #[test]
    fn adapter_event_payload_includes_durable_app_run_identity() {
        let run_id = Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("compile".to_string()),
            message: Some("compile ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("app_run_id".to_string(), json!("adapter-claimed-run")),
                ("node_id".to_string(), json!("adapter_claimed_node")),
                ("attempt".to_string(), json!(1)),
                ("status".to_string(), json!("ok")),
            ]),
        };

        let payload = adapter_event_payload(run_id, dag_run_id, &event);

        assert_eq!(
            payload["app_run_id"],
            json!("2d0a1d88-b9f9-4e8f-848e-605b86717330")
        );
        assert_eq!(payload["dag_run_id"], json!(dag_run_id.to_string()));
        assert_eq!(payload["node_id"], json!("compile"));
        assert_eq!(payload["attempt"], json!(1));
        assert_eq!(payload["status"], json!("ok"));
        for field in [
            "node_kind",
            "tool_id",
            "manifest_hash",
            "artifact_id",
            "lease_id",
            "exit_status",
            "duration_ms",
        ] {
            assert!(
                payload.get(field).is_some(),
                "adapter event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn adapter_stderr_payload_includes_durable_app_run_identity() {
        let run_id = Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();

        let payload = adapter_stderr_payload(run_id, dag_run_id, true);

        assert_eq!(
            payload["app_run_id"],
            json!("2d0a1d88-b9f9-4e8f-848e-605b86717330")
        );
        assert_eq!(payload["dag_run_id"], json!(dag_run_id.to_string()));
        assert_eq!(payload["source"], json!("adapter_stderr"));
        assert_eq!(payload["truncated"], json!(true));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                payload.get(*field).is_some(),
                "adapter stderr payload should include mandatory AgentHero trace field `{field}`"
            );
        }
        assert_eq!(payload["node_id"], serde_json::Value::Null);
        assert_eq!(payload["attempt"], serde_json::Value::Null);
        assert_eq!(payload["exit_status"], serde_json::Value::Null);
        assert_eq!(payload["duration_ms"], serde_json::Value::Null);
    }

    #[test]
    fn adapter_event_persist_budget_caps_rows_and_bytes() {
        let mut budget = AdapterEventPersistBudget {
            rows: 2,
            bytes: 10,
            truncation_recorded: false,
        };

        assert_eq!(budget.take(4), Some(()));
        assert_eq!(budget.rows, 1);
        assert_eq!(budget.bytes, 6);
        assert_eq!(budget.take(6), Some(()));
        assert_eq!(budget.rows, 0);
        assert_eq!(budget.bytes, 0);
        assert_eq!(budget.take(1), None);
    }

    #[test]
    fn adapter_event_persist_budget_rejects_oversized_event() {
        let mut budget = AdapterEventPersistBudget {
            rows: 2,
            bytes: 5,
            truncation_recorded: false,
        };

        assert_eq!(budget.take(6), None);
        assert_eq!(budget.rows, 1);
        assert_eq!(budget.bytes, 0);
        assert_eq!(budget.take(1), None);
    }

    #[test]
    fn bounded_log_line_truncates_long_adapter_stderr() {
        let (short, short_truncated) = bounded_log_line("plain adapter stderr");
        assert_eq!(short, "plain adapter stderr");
        assert!(!short_truncated);

        let (long, long_truncated) = bounded_log_line(&"x".repeat(4_200));
        assert!(long_truncated);
        assert!(long.ends_with("...<truncated>"));
        assert!(long.len() < 4_200);
    }

    #[test]
    fn adapter_log_persist_budget_caps_rows_and_bytes() {
        let mut budget = AdapterLogPersistBudget::default();

        assert!(budget.take("abc").is_some());
        budget.rows = MAX_ADAPTER_LOG_EVENT_ROWS;
        assert!(budget.take("still ignored").is_none());

        let mut byte_budget = AdapterLogPersistBudget::default();
        byte_budget.bytes = MAX_ADAPTER_LOG_EVENT_BYTES.saturating_sub(2);
        assert_eq!(byte_budget.take("abcd").as_deref(), Some("ab"));
        assert_eq!(byte_budget.rows, 1);
        assert_eq!(byte_budget.bytes, MAX_ADAPTER_LOG_EVENT_BYTES);
        assert!(byte_budget.take("more").is_none());
    }

    #[tokio::test]
    async fn cancellation_cleanup_joins_adapter_persisters() {
        let run_id = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let event_joined = Arc::new(AtomicBool::new(false));
        let log_joined = Arc::new(AtomicBool::new(false));

        let event_flag = Arc::clone(&event_joined);
        let event_task = tokio::spawn(async move {
            event_flag.store(true, Ordering::SeqCst);
            AdapterObservedLifecycle::default()
        });
        let log_flag = Arc::clone(&log_joined);
        let log_task = tokio::spawn(async move {
            log_flag.store(true, Ordering::SeqCst);
        });

        let observed = join_adapter_persist_tasks(run_id, event_task, log_task).await;

        assert!(event_joined.load(Ordering::SeqCst));
        assert!(log_joined.load(Ordering::SeqCst));
        assert_eq!(observed, AdapterObservedLifecycle::default());
    }

    #[tokio::test]
    async fn cancellation_cleanup_does_not_wait_forever_for_stuck_adapter_persisters() {
        let run_id = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
        let event_task =
            tokio::spawn(async { std::future::pending::<AdapterObservedLifecycle>().await });
        let log_task = tokio::spawn(async {
            std::future::pending::<()>().await;
        });

        tokio::time::timeout(
            Duration::from_millis(600),
            join_adapter_persist_tasks(run_id, event_task, log_task),
        )
        .await
        .expect("adapter persister cleanup must be bounded");
    }

    #[test]
    fn app_run_log_line_append_creates_parent_dirs_and_file() {
        let run_id = Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-scheduler-log-test-{run_id}"));
        let path = dir.join("nested").join("run.log");

        append_app_run_log_line(&path, "2026-06-18T17:40:00Z info app_run.started")
            .expect("append log line");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "2026-06-18T17:40:00Z info app_run.started\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adapter_event_log_writes_structured_agenthero_event_line() {
        let run_id = Uuid::new_v4();
        let dag_run_id = Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-structured-log-test-{run_id}"));
        let path = dir.join("nested").join("run.log");
        let event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("lean_check".to_string()),
            message: Some("lean_check ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("app_run_id".to_string(), json!("adapter-sent-wrong-run")),
                ("dag_run_id".to_string(), json!("adapter-sent-wrong-dag")),
                ("attempt".to_string(), json!(2)),
                ("status".to_string(), json!("ok")),
                ("kind".to_string(), json!("lean")),
                ("tool".to_string(), json!("lake")),
                ("latency_ms".to_string(), json!(35)),
                ("exit_status".to_string(), json!(0)),
            ]),
        };

        append_adapter_event_log(&path, run_id, dag_run_id, &event);

        let text = std::fs::read_to_string(&path).expect("read structured app-run log");
        assert!(text.contains("node.completed"));
        let structured_line = text
            .lines()
            .find_map(|line| line.strip_prefix(agenthero_agent_runtime::APP_ADAPTER_EVENT_PREFIX))
            .expect("structured AgentHero event log line");
        let emitted: agenthero_dag_executor::DagExecutionEvent =
            serde_json::from_str(structured_line).expect("structured event JSON");
        assert_eq!(emitted.event_type, "node.completed");
        assert_eq!(emitted.node_id.as_deref(), Some("lean_check"));
        assert_eq!(emitted.payload["app_run_id"], json!(run_id.to_string()));
        assert_eq!(emitted.payload["dag_run_id"], json!(dag_run_id.to_string()));
        assert_eq!(emitted.payload["node_kind"], json!("lean"));
        assert_eq!(emitted.payload["tool_id"], json!("lake"));
        assert_eq!(emitted.payload["duration_ms"], json!(35));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                emitted.payload.contains_key(*field),
                "structured log event should include mandatory AgentHero trace field `{field}`"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
