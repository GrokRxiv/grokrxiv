use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    agenthero_orchestrator::entrypoint::run_process().await
}
