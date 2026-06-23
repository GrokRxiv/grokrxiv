//! Tokio scheduler workers for queued AgentHero app runs.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app_runs::{self, ClaimedAppRun};

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
    prepare_app_run_worker_input(&mut run.input.input, run.id, stream_stderr, debug_logs);
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
    prepare_app_run_worker_input(&mut run.input.input, run.id, false, false);
    let idempotency_key = app_run_idempotency_key(run.id);
    let log_path = crate::dag_apps::app_run_log_path(run.id);
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
        json!({
            "app": run.app_id,
            "action": run.action_id,
            "attempt": run.attempt,
            "idempotency_key": idempotency_key.clone(),
            "log_path": log_path.to_string_lossy(),
            "retry": { "max_attempts": run.input.retry.max_attempts },
        }),
    )
    .await?;

    let response = crate::dag_apps::run_app_action_with_idempotency_key(
        &run.app_id,
        &run.action_id,
        run.input.args.clone(),
        run.input.input.clone(),
        run.input.json,
        run.input.dry_run,
        idempotency_key,
    );
    tokio::pin!(response);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    let response = loop {
        tokio::select! {
            result = &mut response => break result,
            _ = heartbeat.tick() => {
                if let Err(err) = app_runs::heartbeat_worker(pool, run.worker_id).await {
                    tracing::warn!(err = %err, run_id = %run.id, "failed to refresh app-run worker heartbeat");
                }
                if let Err(err) = app_runs::renew_lease(pool, run.lease_id).await {
                    tracing::warn!(err = %err, run_id = %run.id, "failed to renew app-run lease");
                }
            }
        }
    };

    match response {
        Ok(response) if response.ok => {
            let output = serde_json::to_value(&response)?;
            app_runs::complete_success(pool, run.id, output, response.report.as_ref()).await?;
            app_runs::release_lease(pool, run.lease_id, "released").await?;
            append_app_run_log_event(&log_path, "info", "app_run.finished", "app run finished");
        }
        Ok(response) => {
            let message = response
                .error
                .unwrap_or_else(|| "app adapter returned ok=false".to_string());
            app_runs::complete_failure(pool, run.id, "failed", "adapter_failed", &message, true)
                .await?;
            app_runs::release_lease(pool, run.lease_id, "failed").await?;
            append_app_run_log_event(&log_path, "error", "app_run.failed", &message);
        }
        Err(err) => {
            let message = format!("{err:#}");
            app_runs::complete_failure(
                pool,
                run.id,
                "system_failed",
                "adapter_system_failed",
                &message,
                true,
            )
            .await?;
            app_runs::release_lease(pool, run.lease_id, "failed").await?;
            append_app_run_log_event(&log_path, "error", "app_run.failed", &message);
        }
    }
    Ok(())
}

fn append_app_run_log_event(path: &Path, level: &str, event: &str, message: &str) {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let line = format!("{timestamp} {level:<5} {event:<28} {message}");
    if let Err(err) = append_app_run_log_line(path, &line) {
        tracing::warn!(err = %err, path = %path.display(), "failed to append app-run log line");
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
    stream_stderr: bool,
    debug_logs: bool,
) {
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

fn idle_sleep_duration(base: Duration, idle_polls: u32) -> Duration {
    let multiplier = idle_polls.clamp(1, 15);
    base.saturating_mul(multiplier).min(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn app_run_worker_input_always_carries_durable_log_path() {
        let run_id = Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let mut input = agenthero_dag_executor::DagIo::default();

        prepare_app_run_worker_input(&mut input, run_id, false, false);

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
}
