//! DB-backed app-run repository and HTTP DTOs.

use agenthero_dag_executor::{DagExecutionReport, DagIo};
use agenthero_dag_runtime::DagNodeStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Request body for enqueueing an app action run.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppRunRequest {
    /// Action-specific args after the app command path.
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial DAG input.
    #[serde(default)]
    pub input: DagIo,
    /// Whether this run should be plan-only.
    #[serde(default)]
    pub dry_run: bool,
    /// Whether the adapter should emit JSON-oriented output.
    #[serde(default = "default_json")]
    pub json: bool,
}

/// Stored app-run input payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredAppRunInput {
    /// Action-specific args after the app command path.
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial DAG input.
    #[serde(default)]
    pub input: DagIo,
    /// Whether this run should be plan-only.
    #[serde(default)]
    pub dry_run: bool,
    /// Whether the adapter should emit JSON-oriented output.
    #[serde(default = "default_json")]
    pub json: bool,
    /// Scheduler retry policy captured when the run was queued.
    #[serde(default)]
    pub retry: StoredAppRunRetry,
}

/// Stored retry policy for one app run.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct StoredAppRunRetry {
    /// Maximum worker attempts before auto-retry stops.
    pub max_attempts: i32,
}

impl Default for StoredAppRunRetry {
    fn default() -> Self {
        Self { max_attempts: 2 }
    }
}

/// App-run row returned by list/detail APIs.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunRecord {
    /// Run id.
    pub id: Uuid,
    /// Product app id.
    pub app_id: String,
    /// App action id.
    pub action_id: String,
    /// Run state.
    pub state: String,
    /// Stored input.
    pub input: serde_json::Value,
    /// Stored output.
    pub output: serde_json::Value,
    /// Optional error code.
    pub error_code: Option<String>,
    /// Optional error message.
    pub error_message: Option<String>,
    /// Optional retryability marker.
    pub error_retryable: Option<bool>,
    /// Number of worker attempts that have claimed this run.
    pub attempt: i32,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Start timestamp.
    pub started_at: Option<DateTime<Utc>>,
    /// Finish timestamp.
    pub finished_at: Option<DateTime<Utc>>,
}

/// Claimed app run for scheduler workers.
#[derive(Debug, Clone)]
pub struct ClaimedAppRun {
    /// Run id.
    pub id: Uuid,
    /// Worker id that claimed this run.
    pub worker_id: Uuid,
    /// Product app id.
    pub app_id: String,
    /// App action id.
    pub action_id: String,
    /// Stored run input.
    pub input: StoredAppRunInput,
    /// Worker lease id.
    pub lease_id: Uuid,
    /// Attempt number assigned to this claim.
    pub attempt: i32,
}

/// App-run event row.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunEvent {
    /// Event id.
    pub id: i64,
    /// Event level.
    pub level: String,
    /// Event type.
    pub event_type: String,
    /// Optional human message.
    pub message: Option<String>,
    /// Event payload.
    pub payload: serde_json::Value,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Decision for an expired worker lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredLeaseDecision {
    /// Requeue the run for one more worker attempt.
    Requeue,
    /// Mark the run as a terminal system failure.
    SystemFailed,
}

/// Result of one expired-lease recovery pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseRecoverySummary {
    /// App runs requeued for one more attempt.
    pub requeued: usize,
    /// App runs marked `system_failed`.
    pub system_failed: usize,
}

/// Decide how to handle an expired lease for a run that has already started
/// `attempt` worker attempts.
pub fn expired_lease_decision(attempt: i32, max_attempts: i32) -> ExpiredLeaseDecision {
    if attempt < max_attempts.max(1) {
        ExpiredLeaseDecision::Requeue
    } else {
        ExpiredLeaseDecision::SystemFailed
    }
}

fn default_json() -> bool {
    true
}

/// Insert a queued app run.
pub async fn insert_queued(
    pool: &PgPool,
    app_id: &str,
    action_id: &str,
    request: AppRunRequest,
) -> anyhow::Result<Uuid> {
    let retry_policy = crate::dag_apps::app_action_retry_policy(app_id, action_id)?;
    let input = serde_json::to_value(StoredAppRunInput {
        args: request.args,
        input: request.input,
        dry_run: request.dry_run,
        json: request.json,
        retry: StoredAppRunRetry {
            max_attempts: retry_policy.max_attempts,
        },
    })?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "insert into app_runs (app_id, action_id, state, input) \
         values ($1, $2, 'queued', $3) returning id",
    )
    .bind(app_id)
    .bind(action_id)
    .bind(input)
    .fetch_one(pool)
    .await?;
    insert_event(
        pool,
        id,
        "info",
        "app_run.queued",
        Some("app run queued"),
        json!({ "retry": { "max_attempts": retry_policy.max_attempts } }),
    )
    .await?;
    Ok(id)
}

/// Register or refresh a worker node row.
pub async fn register_worker(pool: &PgPool, name: &str) -> anyhow::Result<Uuid> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "insert into worker_nodes (name, capabilities, state, last_heartbeat_at) \
         values ($1, '{}'::jsonb, 'online', now()) \
         on conflict (name) do update set \
           state = 'online', last_heartbeat_at = now(), updated_at = now() \
         returning id",
    )
    .bind(name)
    .fetch_one(pool)
    .await?)
}

/// Refresh a worker heartbeat while it is polling or executing work.
pub async fn heartbeat_worker(pool: &PgPool, worker_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "update worker_nodes set state = 'online', last_heartbeat_at = now(), updated_at = now() \
         where id = $1",
    )
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Extend an active app-run lease for long-running app actions.
pub async fn renew_lease(pool: &PgPool, lease_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "update worker_leases \
         set leased_until = now() + interval '15 minutes', updated_at = now() \
         where id = $1 and state = 'leased'",
    )
    .bind(lease_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Claim the next queued app run for a worker.
pub async fn claim_next(pool: &PgPool, worker_id: Uuid) -> anyhow::Result<Option<ClaimedAppRun>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "select id, app_id, action_id, input, attempt \
         from app_runs \
         where state = 'queued' \
           and attempt < coalesce((input #>> '{retry,max_attempts}')::int, 2) \
         order by created_at asc \
         for update skip locked \
         limit 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };

    let id: Uuid = row.get("id");
    let attempt = row.get::<i32, _>("attempt") + 1;
    sqlx::query(
        "update app_runs set state = 'running', attempt = attempt + 1, \
         started_at = coalesce(started_at, now()), finished_at = null, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let lease_id = sqlx::query_scalar::<_, Uuid>(
        "insert into worker_leases (worker_id, app_run_id, state, leased_until) \
         values ($1, $2, 'leased', now() + interval '15 minutes') returning id",
    )
    .bind(worker_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let input_value: serde_json::Value = row.get("input");
    let input: StoredAppRunInput = serde_json::from_value(input_value)?;
    Ok(Some(ClaimedAppRun {
        id,
        worker_id,
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        input,
        lease_id,
        attempt,
    }))
}

/// Claim one specific queued app run for a worker.
pub async fn claim_run(
    pool: &PgPool,
    worker_id: Uuid,
    run_id: Uuid,
) -> anyhow::Result<Option<ClaimedAppRun>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "select id, app_id, action_id, input, attempt \
         from app_runs \
         where id = $1 \
           and state = 'queued' \
           and attempt < coalesce((input #>> '{retry,max_attempts}')::int, 2) \
         for update skip locked",
    )
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };

    let id: Uuid = row.get("id");
    let attempt = row.get::<i32, _>("attempt") + 1;
    sqlx::query(
        "update app_runs set state = 'running', attempt = attempt + 1, \
         started_at = coalesce(started_at, now()), finished_at = null, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let lease_id = sqlx::query_scalar::<_, Uuid>(
        "insert into worker_leases (worker_id, app_run_id, state, leased_until) \
         values ($1, $2, 'leased', now() + interval '15 minutes') returning id",
    )
    .bind(worker_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let input_value: serde_json::Value = row.get("input");
    let input: StoredAppRunInput = serde_json::from_value(input_value)?;
    Ok(Some(ClaimedAppRun {
        id,
        worker_id,
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        input,
        lease_id,
        attempt,
    }))
}

/// Recover app runs whose worker lease expired while still marked running.
///
/// Runs are requeued until the stored action retry budget is exhausted; after
/// that they become `system_failed` and remain retryable by explicit operator
/// action.
pub async fn recover_expired_leases(pool: &PgPool) -> anyhow::Result<LeaseRecoverySummary> {
    let requeued_rows = sqlx::query(
        "with expired as ( \
           update worker_leases wl set state = 'expired', updated_at = now() \
           from app_runs ar \
           where wl.app_run_id = ar.id \
             and wl.state = 'leased' \
             and wl.leased_until < now() \
             and ar.state = 'running' \
             and ar.attempt < coalesce((ar.input #>> '{retry,max_attempts}')::int, 2) \
           returning wl.app_run_id \
         ) \
         update app_runs ar set state = 'queued', recovered_at = now(), \
           last_lease_expired_at = now(), updated_at = now(), \
           error_code = 'lease_expired', \
           error_message = 'worker lease expired; run requeued', \
           error_retryable = true \
         from expired \
         where ar.id = expired.app_run_id \
         returning ar.id",
    )
    .fetch_all(pool)
    .await?;

    let failed_rows = sqlx::query(
        "with expired as ( \
           update worker_leases wl set state = 'expired', updated_at = now() \
           from app_runs ar \
           where wl.app_run_id = ar.id \
             and wl.state = 'leased' \
             and wl.leased_until < now() \
             and ar.state = 'running' \
             and ar.attempt >= coalesce((ar.input #>> '{retry,max_attempts}')::int, 2) \
           returning wl.app_run_id \
         ) \
         update app_runs ar set state = 'system_failed', finished_at = coalesce(finished_at, now()), \
           last_lease_expired_at = now(), updated_at = now(), \
           error_code = 'lease_expired', \
           error_message = 'worker lease expired after retry', \
           error_retryable = true \
         from expired \
         where ar.id = expired.app_run_id \
         returning ar.id",
    )
    .fetch_all(pool)
    .await?;

    for row in &requeued_rows {
        let run_id: Uuid = row.get("id");
        insert_event(
            pool,
            run_id,
            "warn",
            "app_run.lease_expired_requeued",
            Some("worker lease expired; app run requeued"),
            json!({ "decision": "requeue" }),
        )
        .await?;
    }
    for row in &failed_rows {
        let run_id: Uuid = row.get("id");
        insert_event(
            pool,
            run_id,
            "error",
            "app_run.lease_expired_failed",
            Some("worker lease expired after retry"),
            json!({ "decision": "system_failed" }),
        )
        .await?;
    }

    Ok(LeaseRecoverySummary {
        requeued: requeued_rows.len(),
        system_failed: failed_rows.len(),
    })
}

/// Persist a successful adapter response.
pub async fn complete_success(
    pool: &PgPool,
    run_id: Uuid,
    output: serde_json::Value,
    report: Option<&DagExecutionReport>,
) -> anyhow::Result<()> {
    let state = report.map(report_state).unwrap_or("done");
    sqlx::query(
        "update app_runs set state = $2, output = $3, finished_at = now(), updated_at = now(), \
         error_code = null, error_message = null, error_retryable = null where id = $1",
    )
    .bind(run_id)
    .bind(state)
    .bind(output)
    .execute(pool)
    .await?;
    if let Some(report) = report {
        persist_dag_report(pool, run_id, report).await?;
    }
    insert_event(
        pool,
        run_id,
        "info",
        "app_run.finished",
        Some("app run finished"),
        json!({ "state": state }),
    )
    .await?;
    Ok(())
}

/// Persist a failed adapter response or system failure.
pub async fn complete_failure(
    pool: &PgPool,
    run_id: Uuid,
    state: &str,
    code: &str,
    message: &str,
    retryable: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "update app_runs set state = $2, error_code = $3, error_message = $4, \
         error_retryable = $5, finished_at = now(), updated_at = now() where id = $1",
    )
    .bind(run_id)
    .bind(state)
    .bind(code)
    .bind(message)
    .bind(retryable)
    .execute(pool)
    .await?;
    insert_event(
        pool,
        run_id,
        "error",
        "app_run.failed",
        Some(message),
        json!({ "state": state, "code": code, "retryable": retryable }),
    )
    .await?;
    Ok(())
}

/// Mark a worker lease released.
pub async fn release_lease(pool: &PgPool, lease_id: Uuid, state: &str) -> anyhow::Result<()> {
    sqlx::query("update worker_leases set state = $2, updated_at = now() where id = $1")
        .bind(lease_id)
        .bind(state)
        .execute(pool)
        .await?;
    Ok(())
}

/// List app runs.
pub async fn list_runs(
    pool: &PgPool,
    app: Option<&str>,
    state: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<AppRunRecord>> {
    let rows = sqlx::query(
        "select id, app_id, action_id, state, input, output, error_code, error_message, \
                error_retryable, attempt, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
           and ($2::text is null or state = $2) \
         order by created_at desc \
         limit $3",
    )
    .bind(app)
    .bind(state)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(record_from_row).collect())
}

/// Fetch one app run.
pub async fn get_run(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Option<AppRunRecord>> {
    let row = sqlx::query(
        "select id, app_id, action_id, state, input, output, error_code, error_message, \
                error_retryable, attempt, created_at, started_at, finished_at \
         from app_runs where id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(record_from_row))
}

/// List app-run events.
pub async fn list_events(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Vec<AppRunEvent>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from dag_events where app_run_id = $1 order by created_at asc, id asc",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunEvent {
            id: row.get("id"),
            level: row.get("level"),
            event_type: row.get("event_type"),
            message: row.get("message"),
            payload: row.get("payload"),
            created_at: row.get("created_at"),
        })
        .collect())
}

/// Insert an app-run event.
pub async fn insert_event(
    pool: &PgPool,
    run_id: Uuid,
    level: &str,
    event_type: &str,
    message: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into dag_events (app_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_dag_report(
    pool: &PgPool,
    app_run_id: Uuid,
    report: &DagExecutionReport,
) -> anyhow::Result<()> {
    let dag_state = report_state(report);
    let dag_run_id = sqlx::query_scalar::<_, Uuid>(
        "insert into dag_runs (app_run_id, dag_type, state, output, started_at, finished_at) \
         values ($1, $2, $3, $4, now(), now()) returning id",
    )
    .bind(app_run_id)
    .bind(report.dag_type.as_str())
    .bind(dag_state)
    .bind(serde_json::to_value(&report.outputs)?)
    .fetch_one(pool)
    .await?;

    for node in &report.nodes {
        sqlx::query(
            "insert into dag_run_nodes \
             (dag_run_id, node_id, node_kind, state, runner, input, output, error_message, latency_ms, started_at, finished_at) \
             values ($1, $2, $3, $4, $5, '{}'::jsonb, $6, $7, $8, now(), now())",
        )
        .bind(dag_run_id)
        .bind(&node.node_id)
        .bind(&node.kind)
        .bind(node_state(node.status))
        .bind(node.executor.as_deref())
        .bind(json!({
            "inputs": node.inputs,
            "outputs": node.outputs,
            "warning": node.warning,
            "trace": node.trace,
        }))
        .bind(node.error.as_deref())
        .bind(node.latency_ms.map(|value| {
            i32::try_from(value).unwrap_or(i32::MAX)
        }))
        .execute(pool)
        .await?;
    }
    Ok(())
}

fn record_from_row(row: sqlx::postgres::PgRow) -> AppRunRecord {
    AppRunRecord {
        id: row.get("id"),
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        state: row.get("state"),
        input: row.get("input"),
        output: row.get("output"),
        error_code: row.get("error_code"),
        error_message: row.get("error_message"),
        error_retryable: row.get("error_retryable"),
        attempt: row.get("attempt"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn report_state(report: &DagExecutionReport) -> &'static str {
    match report.status {
        DagNodeStatus::Ok => "done",
        DagNodeStatus::Degraded | DagNodeStatus::Skipped => "partial",
        DagNodeStatus::Pending | DagNodeStatus::Running => "running",
        DagNodeStatus::AwaitingApproval => "awaiting_approval",
        DagNodeStatus::Failed => "failed",
    }
}

fn node_state(status: DagNodeStatus) -> &'static str {
    match status {
        DagNodeStatus::Pending => "queued",
        DagNodeStatus::Running => "running",
        DagNodeStatus::AwaitingApproval => "awaiting_approval",
        DagNodeStatus::Ok => "ok",
        DagNodeStatus::Degraded => "degraded",
        DagNodeStatus::Failed => "failed",
        DagNodeStatus::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_lease_recovery_uses_action_retry_budget() {
        assert_eq!(expired_lease_decision(1, 3), ExpiredLeaseDecision::Requeue);
        assert_eq!(expired_lease_decision(2, 3), ExpiredLeaseDecision::Requeue);
        assert_eq!(
            expired_lease_decision(3, 3),
            ExpiredLeaseDecision::SystemFailed
        );
        assert_eq!(
            expired_lease_decision(4, 3),
            ExpiredLeaseDecision::SystemFailed
        );
    }
}
