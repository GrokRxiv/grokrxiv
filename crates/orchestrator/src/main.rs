//! GrokRxiv orchestrator binary entry point.
//!
//! The CLI surface (subcommands, flags, --help) lives in `cli.rs`. This file
//! only handles boot wiring and process exit.

use clap::Parser;
use grokrxiv_orchestrator::cli::{run, Cli};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(err = %e, "command failed");
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,grokrxiv_orchestrator=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .init();
}
