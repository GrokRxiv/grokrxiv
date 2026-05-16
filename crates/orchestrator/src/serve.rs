//! Run the long-running HTTP API + supervisor + scheduler.
//!
//! Extracted from `main.rs` so the CLI's `serve` subcommand and the binary's
//! default invocation share the exact same code path.

use std::net::SocketAddr;

use crate::scheduler::Scheduler;
use crate::supervisor::Supervisor;
use crate::{router, AppState, Config};

/// Start the orchestrator and block forever.
pub async fn run() -> anyhow::Result<()> {
    let config = Config::from_env();
    let bind: SocketAddr = config.bind.parse()?;
    let scheduler_cfg = config.scheduler.clone();
    let state = AppState::from_config(config).await?;
    let app = router(state.clone());

    let supervisor = Supervisor::spawn(state.clone());
    let _scheduler = Scheduler::spawn(supervisor.sender(), scheduler_cfg);
    std::mem::drop(supervisor);

    tracing::info!(addr = %bind, "orchestrator listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
