//! Run the long-running HTTP API + supervisor + scheduler.
//!
//! Extracted from `main.rs` so every launcher can call the explicit `serve`
//! subcommand without duplicating boot logic.

use std::net::SocketAddr;

use crate::scheduler::Scheduler;
use crate::supervisor::Supervisor;
use crate::{router, AppState, Config};

fn scheduler_disabled_from_env() -> bool {
    scheduler_disabled_from_env_value(std::env::var("GROKRXIV_DISABLE_SCHEDULER").ok().as_deref())
}

fn scheduler_disabled_from_env_value(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn should_spawn_scheduler(db_configured: bool, scheduler_disabled: bool) -> bool {
    db_configured && !scheduler_disabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_disable_env_accepts_common_truthy_values() {
        assert!(scheduler_disabled_from_env_value(Some("1")));
        assert!(scheduler_disabled_from_env_value(Some("true")));
        assert!(scheduler_disabled_from_env_value(Some("yes")));
        assert!(!scheduler_disabled_from_env_value(Some("0")));
        assert!(!scheduler_disabled_from_env_value(Some("false")));
        assert!(!scheduler_disabled_from_env_value(None));
    }

    #[test]
    fn scheduler_does_not_spawn_in_stateless_mode() {
        assert!(!should_spawn_scheduler(false, false));
        assert!(!should_spawn_scheduler(false, true));
        assert!(!should_spawn_scheduler(true, true));
        assert!(should_spawn_scheduler(true, false));
    }
}

/// Start the orchestrator and block forever.
pub async fn run() -> anyhow::Result<()> {
    let config = Config::from_env();
    let bind: SocketAddr = config.bind.parse()?;
    let scheduler_cfg = config.scheduler.clone();
    let state = AppState::from_config(config).await?;
    let app = router(state.clone());

    let supervisor = Supervisor::spawn(state.clone());
    #[cfg(feature = "grokrxiv-publisher")]
    crate::supervisor::spawn_publish_reconcile(state.clone());
    let scheduler_disabled = scheduler_disabled_from_env();
    let scheduler = if !should_spawn_scheduler(state.db.is_some(), scheduler_disabled) {
        if scheduler_disabled {
            tracing::info!("scheduler disabled by GROKRXIV_DISABLE_SCHEDULER");
        } else {
            tracing::warn!("scheduler disabled because DATABASE_URL is not configured");
        }
        None
    } else {
        Some(Scheduler::spawn(supervisor.sender(), scheduler_cfg))
    };
    let shutdown_supervisor = supervisor.clone();
    let shutdown = async move {
        wait_for_shutdown_signal().await;
        shutdown_supervisor.shutdown();
    };

    tracing::info!(addr = %bind, "orchestrator listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    if let Some(handle) = scheduler {
        handle.abort();
    }
    supervisor.shutdown();
    Ok(())
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(err = %e, "failed to install Ctrl-C shutdown handler");
        }
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(e) => {
                    tracing::warn!(err = %e, "failed to install SIGTERM shutdown handler");
                    std::future::pending::<()>().await;
                }
            }
        };

        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }

    tracing::info!("shutdown signal received");
}
