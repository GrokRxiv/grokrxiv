//! Compatibility binary alias for the primary `agh` CLI.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    agenthero_orchestrator::entrypoint::run_process().await
}
