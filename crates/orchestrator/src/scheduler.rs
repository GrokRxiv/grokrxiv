//! Tokio scheduler workers for queued AgentHero app runs.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqlx::PgPool;

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
            Some(run) => execute_claimed(&pool, run).await?,
            None => tokio::time::sleep(interval).await,
        }
    }
}

async fn execute_claimed(pool: &PgPool, run: ClaimedAppRun) -> anyhow::Result<()> {
    app_runs::insert_event(
        pool,
        run.id,
        "info",
        "app_run.started",
        Some("app run started"),
        json!({ "app": run.app_id, "action": run.action_id, "attempt": run.attempt }),
    )
    .await?;

    let response = crate::dag_apps::run_app_action(
        &run.app_id,
        &run.action_id,
        run.input.args.clone(),
        run.input.input.clone(),
        run.input.json,
        run.input.dry_run,
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
