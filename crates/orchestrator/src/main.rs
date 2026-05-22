//! GrokRxiv orchestrator binary entry point.
//!
//! The CLI surface (subcommands, flags, --help) lives in `cli.rs`. This file
//! only handles boot wiring and process exit.

use clap::{error::ErrorKind, Parser};
use grokrxiv_orchestrator::cli::{run, Cli, Command};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = grokrxiv_orchestrator::config::load_env() {
        eprintln!("error: {err:#}");
        return ExitCode::from(1);
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
        Err(e) => {
            tracing::error!(err = %e, "command failed");
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing(cli: &Cli) {
    if tracing_mode(cli) == TracingMode::Structured {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,grokrxiv_orchestrator=debug"));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TracingMode {
    Structured,
    Silent,
}

fn tracing_mode(cli: &Cli) -> TracingMode {
    if cli.debug_logs || matches!(&cli.command, Command::Serve) {
        TracingMode::Structured
    } else {
        TracingMode::Silent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn foreground_cli_tracing_ignores_debug_env_without_flag() {
        std::env::set_var("GROKRXIV_DEBUG_LOGS", "1");
        let cli = Cli::try_parse_from(["grokrxiv", "doctor"]).unwrap();

        assert_eq!(tracing_mode(&cli), TracingMode::Silent);

        std::env::remove_var("GROKRXIV_DEBUG_LOGS");
    }

    #[test]
    fn debug_logs_flag_enables_structured_cli_tracing() {
        let cli = Cli::try_parse_from(["grokrxiv", "--debug-logs", "doctor"]).unwrap();

        assert_eq!(tracing_mode(&cli), TracingMode::Structured);
    }

    #[test]
    fn serve_keeps_structured_service_tracing() {
        let cli = Cli::try_parse_from(["grokrxiv", "serve"]).unwrap();

        assert_eq!(tracing_mode(&cli), TracingMode::Structured);
    }
}
