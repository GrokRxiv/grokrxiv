//! Tokio scheduler workers for queued AgentHero app runs.

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

async fn worker_loop(pool: Arc<PgPool>, name: String, interval: Duration) -> anyhow::Result<()> {
    let worker_id = app_runs::register_worker(&pool, &name).await?;
    let mut idle_polls = 0u32;
    loop {
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

async fn execute_claimed(pool: &PgPool, run: ClaimedAppRun) -> anyhow::Result<()> {
    let idempotency_key = app_run_idempotency_key(run.id);
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
    )
    .await;

    match response {
        Ok(response) if response.ok => {
            let output = serde_json::to_value(&response)?;
            app_runs::complete_success(pool, run.id, output, response.report.as_ref()).await?;
            app_runs::release_lease(pool, run.lease_id, "released").await?;
        }
        Ok(response) => {
            let message = response
                .error
                .unwrap_or_else(|| "app adapter returned ok=false".to_string());
            app_runs::complete_failure(pool, run.id, "failed", "adapter_failed", &message, true)
                .await?;
            app_runs::release_lease(pool, run.lease_id, "failed").await?;
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
        }
    }
    Ok(())
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
}
