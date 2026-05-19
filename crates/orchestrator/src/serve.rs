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
}

/// Start the orchestrator and block forever.
pub async fn run() -> anyhow::Result<()> {
    let config = Config::from_env();
    let bind: SocketAddr = config.bind.parse()?;
    let scheduler_cfg = config.scheduler.clone();
    let state = AppState::from_config(config).await?;
    let app = router(state.clone());

    let supervisor = Supervisor::spawn(state.clone());
    let _scheduler = if scheduler_disabled_from_env() {
        tracing::info!("scheduler disabled by GROKRXIV_DISABLE_SCHEDULER");
        None
    } else {
        Some(Scheduler::spawn(supervisor.sender(), scheduler_cfg))
    };
    std::mem::drop(supervisor);

    tracing::info!(addr = %bind, "orchestrator listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
