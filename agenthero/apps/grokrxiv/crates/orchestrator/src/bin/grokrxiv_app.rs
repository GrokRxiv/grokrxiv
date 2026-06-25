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
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < raw_args.len() {
        let arg = &raw_args[index];
        match arg.as_str() {
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--status" => status = true,
            "--no-status" => no_status = true,
            "--debug-logs" => debug_logs = true,
            "--no-cache" => no_cache = true,
            _ => {
                if apply_runtime_override_arg(arg, raw_args.get(index + 1).map(String::as_str)) {
                    if !arg.contains('=') {
                        index += 1;
                    }
                } else {
                    args.push(arg.clone());
                }
            }
        }
        index += 1;
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

fn apply_runtime_override_arg(arg: &str, next: Option<&str>) -> bool {
    let (flag, inline_value) = arg
        .split_once('=')
        .map_or((arg, None), |(flag, value)| (flag, Some(value)));
    let Some(prefix) = runtime_override_prefix(flag) else {
        return false;
    };
    let Some(raw) = inline_value.or(next) else {
        return true;
    };
    let Some((role, value)) = raw.split_once('=') else {
        return true;
    };
    let role_suffix = grokrxiv_app_runtime::runtime_config::role_env_suffix(role);
    std::env::set_var(format!("{prefix}{role_suffix}"), value);
    true
}

fn runtime_override_prefix(flag: &str) -> Option<&'static str> {
    match flag {
        "--provider-for" => {
            Some(grokrxiv_app_runtime::runtime_config::PROVIDER_OVERRIDE_ENV_PREFIX)
        }
        "--model-for" => Some(grokrxiv_app_runtime::runtime_config::MODEL_OVERRIDE_ENV_PREFIX),
        "--runner-for" => Some("AGENTHERO_RUNNER_OVERRIDE_"),
        _ => None,
    }
}
