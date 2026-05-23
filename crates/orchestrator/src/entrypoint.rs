//! Shared process entrypoint for the `agh` CLI and `agenthero` alias.

use crate::cli::{run, Cli, Command};
use clap::{error::ErrorKind, Parser};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

/// Load environment, parse CLI arguments, initialize tracing, and run a command.
pub async fn run_process() -> ExitCode {
    if let Err(err) = crate::config::load_env() {
        eprintln!("error: {err:#}");
        return ExitCode::from(1);
    }

    match crate::cli::try_print_app_run_help_from_args(std::env::args().skip(1).collect()) {
        Ok(true) => return ExitCode::SUCCESS,
        Ok(false) => {}
        Err(err) => {
            eprintln!("error: {err:#}");
            return ExitCode::from(1);
        }
    }

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) if err.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = err.print();
            return ExitCode::SUCCESS;
        }
        Err(err) => err.exit(),
    };
    init_tracing(&cli);
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(err = %err, "command failed");
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing(cli: &Cli) {
    if cli.debug_logs || matches!(&cli.command, Command::Serve) {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,agenthero_orchestrator=debug"));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(false)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("off"))
            .with_writer(std::io::stderr)
            .init();
    }
}
