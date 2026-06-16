//! App-local GrokRxiv action runner used by the AgentHero adapter.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = grokrxiv_app_runtime::config::load_env() {
        eprintln!("error: {err:#}");
        return ExitCode::from(1);
    }
    let mut json = false;
    let mut dry_run = false;
    let mut status = false;
    let mut no_status = false;
    let mut debug_logs = false;
    let mut no_cache = false;
    let mut args = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--status" => status = true,
            "--no-status" => no_status = true,
            "--debug-logs" => debug_logs = true,
            "--no-cache" => no_cache = true,
            _ => args.push(arg),
        }
    }
    if no_cache {
        std::env::set_var("GROKRXIV_INGEST_NO_CACHE", "1");
        std::env::set_var("GROKRXIV_NO_CACHE", "1");
    }
    grokrxiv_app_runtime::cli_status::set_enabled(status || (!no_status && debug_logs));
    let Some(action) = args.first().cloned() else {
        eprintln!("error: missing GrokRxiv action");
        return ExitCode::from(2);
    };
    // Local dev: run the orchestrator HTTP server (the backend the web app talks to on
    // ORCHESTRATOR_INTERNAL_URL / :8080) directly from the app binary, so the current
    // app-relative DAG/agent layout is used (the legacy root `grokrxiv serve` looks for
    // DAGs at the old repo-root path and panics).
    if action == "serve" {
        return match grokrxiv_app_runtime::serve::run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err:#}");
                ExitCode::from(1)
            }
        };
    }
    let action_args = args.into_iter().skip(1).collect();
    match grokrxiv_app_runtime::cli::run_grokrxiv_action(&action, action_args, json, dry_run).await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}
