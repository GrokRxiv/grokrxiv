use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use clap::{CommandFactory, Parser};
use serde_json::Value;
use tower::ServiceExt;

use agenthero_orchestrator::cli::{AppCommand, Cli, Command};

fn agh(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_agh"))
        .current_dir(workspace_root())
        .args(args)
        .output()
        .expect("run agh test binary")
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn binary_contract_is_agh() {
    let command = Cli::command();
    assert_eq!(command.get_name(), "agh");
    assert!(
        command
            .get_about()
            .map(|about| about.to_string().contains("AgentHero"))
            .unwrap_or(false),
        "top-level help should present AgentHero as the runtime brand"
    );
}

#[test]
fn cargo_installs_agh_primary_and_agenthero_alias() {
    let manifest = std::fs::read_to_string(
        workspace_root()
            .join("crates")
            .join("orchestrator")
            .join("Cargo.toml"),
    )
    .expect("read orchestrator Cargo.toml");

    assert!(manifest.contains("default-run = \"agh\""));
    assert!(manifest.contains("name = \"agh\""));
    assert!(manifest.contains("name = \"agenthero\""));
}

#[test]
fn app_run_grokrxiv_command_parses_as_canonical_app_execution() {
    let cli = Cli::try_parse_from([
        "agh",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2509.09915v1",
    ])
    .expect("app run grokrxiv review command should parse without a separator");

    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["review", "https://arxiv.org/abs/2509.09915v1"]);
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_grokrxiv_dry_run_parses_at_agenthero_layer() {
    let cli = Cli::try_parse_from([
        "agh",
        "--dry-run",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2509.09915v1",
    ])
    .expect("dry-run app review command should parse");

    assert!(cli.dry_run);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["review", "https://arxiv.org/abs/2509.09915v1"]);
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_grokrxiv_review_parses_as_product_path() {
    let cli = Cli::try_parse_from([
        "agh",
        "--json",
        "--dry-run",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2606.00799",
        "--no-external-actions",
    ])
    .expect("review --no-external-actions should parse as the canonical app review path");

    assert!(cli.json);
    assert!(cli.dry_run);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "review",
                        "https://arxiv.org/abs/2606.00799",
                        "--no-external-actions"
                    ]
                );
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_grokrxiv_legacy_loop_flag_still_parses_for_compatibility() {
    let cli = Cli::try_parse_from([
        "agh",
        "--json",
        "--dry-run",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2606.00799",
        "--loop",
        "--no-external-actions",
    ])
    .expect("legacy review --loop flag should still parse as an app argument");

    assert!(cli.json);
    assert!(cli.dry_run);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "review",
                        "https://arxiv.org/abs/2606.00799",
                        "--loop",
                        "--no-external-actions"
                    ]
                );
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_defaults_to_streaming_adapter_status_unless_suppressed() {
    let cli = Cli::try_parse_from([
        "agh",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2606.00799",
    ])
    .expect("review should parse");
    assert!(agenthero_orchestrator::cli::stream_app_stderr_for_cli(&cli));

    let quiet = Cli::try_parse_from([
        "agh",
        "--no-status",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2606.00799",
    ])
    .expect("review --no-status should parse");
    assert!(!agenthero_orchestrator::cli::stream_app_stderr_for_cli(
        &quiet
    ));
}

#[test]
fn grokrxiv_review_action_catalog_declares_observable_review_options_and_dag() {
    let app = std::fs::read_to_string(
        workspace_root()
            .join("agenthero")
            .join("apps")
            .join("grokrxiv")
            .join("app.yaml"),
    )
    .expect("read grokrxiv app manifest");
    let manifest: serde_yaml::Value = serde_yaml::from_str(&app).expect("parse grokrxiv app yaml");
    let actions = manifest
        .get("actions")
        .and_then(|value| value.as_sequence())
        .expect("actions array");
    let review = actions
        .iter()
        .find(|action| action.get("id").and_then(|id| id.as_str()) == Some("review"))
        .expect("review action");

    assert_eq!(
        review.get("dag_type").and_then(|value| value.as_str()),
        Some("review-loop"),
        "review should be the product review path, with paper-review called inside the loop DAG"
    );
    let options = review
        .get("options")
        .and_then(|value| value.as_sequence())
        .expect("review options");
    assert_eq!(
        options
            .iter()
            .find(|option| option.get("name").and_then(|name| name.as_str()) == Some("--loop")),
        None,
        "deprecated no-op --loop should remain parser-compatible but should not be advertised"
    );
    let debug_option = options
        .iter()
        .find(|option| option.get("name").and_then(|name| name.as_str()) == Some("--debug"))
        .expect("review action should advertise --debug");
    assert_eq!(
        debug_option.get("kind").and_then(|value| value.as_str()),
        Some("flag")
    );
    let no_external_actions_option = options
        .iter()
        .find(|option| {
            option.get("name").and_then(|name| name.as_str()) == Some("--no-external-actions")
        })
        .expect("review action should advertise --no-external-actions");
    assert_eq!(
        no_external_actions_option
            .get("kind")
            .and_then(|value| value.as_str()),
        Some("flag")
    );
    let no_lean_option = options
        .iter()
        .find(|option| option.get("name").and_then(|name| name.as_str()) == Some("--no-lean"))
        .expect("review action should advertise --no-lean");
    assert_eq!(
        no_lean_option.get("kind").and_then(|value| value.as_str()),
        Some("flag")
    );
}

#[test]
fn app_show_json_surfaces_agentapp_contracts() {
    let output = agh(&["--json", "app", "show", "c2rust"]);
    assert!(
        output.status.success(),
        "agh app show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let app: Value = serde_json::from_slice(&output.stdout).expect("parse app show json");

    assert_eq!(
        app["contracts"]["state_schemas"],
        serde_json::json!(["state/run_state.schema.json"])
    );
    assert_eq!(app["contracts"]["tools"], serde_json::json!("tools.yaml"));
    assert_eq!(
        app["contracts"]["evals"],
        serde_json::json!(["evals/smoke.yaml"])
    );
    assert_eq!(app["observability"]["events"], serde_json::json!(true));
    assert_eq!(app["observability"]["logs"], serde_json::json!(true));
    assert_eq!(app["observability"]["status"], serde_json::json!(true));
    assert_eq!(
        app["observability"]["event_stream"],
        serde_json::json!(true)
    );
    assert!(app["observability"]["lifecycle_events"]
        .as_array()
        .expect("lifecycle event list")
        .iter()
        .any(|value| value == "app_action.completed"));
    assert!(app["observability"]["trace_fields"]
        .as_array()
        .expect("trace field list")
        .iter()
        .any(|value| value == "manifest_hash"));
}

#[test]
fn app_eval_command_parses_and_lists_app_owned_evals() {
    let cli = Cli::try_parse_from(["agh", "app", "eval", "c2rust", "smoke"])
        .expect("app eval command should parse");
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Eval { app, eval_id } => {
                assert_eq!(app, "c2rust");
                assert_eq!(eval_id.as_deref(), Some("smoke"));
            }
            other => panic!("expected App::Eval command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }

    let output = agh(&["--json", "app", "eval", "c2rust"]);
    assert!(
        output.status.success(),
        "agh app eval failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("parse app eval json");
    assert_eq!(result["app"], "c2rust");
    assert_eq!(result["evals"][0]["id"], "smoke");
    assert_eq!(result["evals"][0]["path"], "evals/smoke.yaml");
}

#[test]
fn review_loop_manifest_declares_full_semantic_fix_publish_flow() {
    let path = workspace_root()
        .join("agenthero")
        .join("apps")
        .join("grokrxiv")
        .join("dags")
        .join("review-loop.yaml");
    let yaml = std::fs::read_to_string(&path).expect("read review-loop manifest");
    let manifest: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse review-loop yaml");
    assert_eq!(
        manifest.get("id").and_then(|value| value.as_str()),
        Some("review-loop")
    );

    let nodes = manifest
        .get("nodes")
        .and_then(|value| value.as_sequence())
        .expect("nodes array");
    let ids = nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(|id| id.as_str()))
        .collect::<Vec<_>>();
    for expected in [
        "paper_review",
        "claim_extractor",
        "paper_math_source_collector",
        "knowledge_graph_builder",
        "semantic_category_mapper",
        "proof_obligation_generator",
        "lean_review_fix_code",
        "lean_faithfulness_check",
        "semantic_adequacy_checker",
        "citation_validation",
        "pr_fixer",
        "pr_review_fix_code",
        "policy_gate",
        "review_loop_report",
        "publish_decision",
    ] {
        assert!(
            ids.contains(&expected),
            "review-loop manifest missing node {expected}; nodes were {ids:?}"
        );
    }
    let roles = manifest
        .get("roles")
        .and_then(|value| value.as_sequence())
        .expect("roles array");
    for expected in [
        "lean_proof_author",
        "lean_code_reviewer",
        "lean_code_fixer",
        "pr_artifact_fixer",
        "pr_artifact_reviewer",
    ] {
        assert!(
            roles
                .iter()
                .any(|role| role.get("id").and_then(|id| id.as_str()) == Some(expected)),
            "review-loop manifest missing role {expected}"
        );
    }

    for loop_id in ["lean_review_fix_code", "pr_review_fix_code"] {
        let node = nodes
            .iter()
            .find(|node| node.get("id").and_then(|id| id.as_str()) == Some(loop_id))
            .unwrap_or_else(|| panic!("{loop_id} node"));
        assert_eq!(
            node.get("kind").and_then(|value| value.as_str()),
            Some("loop")
        );
        assert_eq!(
            node.get("tool").and_then(|value| value.as_str()),
            Some("review_fix_code")
        );
        assert_eq!(
            node.get("loop")
                .and_then(|value| value.get("max_rounds"))
                .and_then(|value| value.as_u64()),
            Some(3)
        );
    }

    let citation = nodes
        .iter()
        .find(|node| node.get("id").and_then(|id| id.as_str()) == Some("citation_validation"))
        .expect("citation_validation node");
    assert_eq!(
        citation.get("kind").and_then(|value| value.as_str()),
        Some("dag_call")
    );
    assert_eq!(
        citation.get("dag_type").and_then(|value| value.as_str()),
        Some("citation-validation")
    );
}

#[test]
fn app_run_grokrxiv_command_parses_without_separator() {
    let cli = Cli::try_parse_from([
        "agh",
        "--json",
        "app",
        "run",
        "grokrxiv",
        "validate",
        "citations",
    ])
    .expect("app run grokrxiv validate citations command should parse without --");

    assert!(cli.json);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["validate", "citations"]);
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_c2rust_command_parses_and_direct_app_commands_do_not() {
    let cli = Cli::try_parse_from([
        "agh",
        "app",
        "run",
        "c2rust",
        "migrate",
        "fixtures/kernel.c",
    ])
    .expect("app run c2rust migrate command should parse without a separator");
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "c2rust");
                assert_eq!(args, vec!["migrate", "fixtures/kernel.c"]);
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }

    assert!(
        Cli::try_parse_from(["agh", "grokrxiv", "review", "2605.17307"]).is_err(),
        "direct grokrxiv app path must not remain callable"
    );
    assert!(
        Cli::try_parse_from(["agh", "c2rust", "migrate", "fixtures/kernel.c"]).is_err(),
        "direct c2rust app path must not remain callable"
    );
    assert!(
        Cli::try_parse_from(["agh", "apps", "list"]).is_err(),
        "legacy top-level `apps` alias must not remain callable"
    );
}

#[test]
fn app_run_with_no_action_is_action_catalog_request() {
    let cli = Cli::try_parse_from(["agh", "--json", "app", "run", "grokrxiv"])
        .expect("app run grokrxiv with no action should parse as app action catalog");

    assert!(cli.json);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert!(args.is_empty());
            }
            other => panic!("expected App::Run command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_app_help_renders_manifest_catalog_without_adapter_or_runner_options() {
    let output = agh(&["app", "run", "grokrxiv", "--help"]);
    assert!(
        output.status.success(),
        "app help should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("GrokRxiv"),
        "help should identify the app, got:\n{stdout}"
    );
    assert!(
        stdout.contains("review") && stdout.contains("validate citations"),
        "help should list manifest-declared actions, got:\n{stdout}"
    );
    assert!(
        stdout.contains("source") && stdout.contains("URL_OR_PATH"),
        "help should expose action options from app.yaml, got:\n{stdout}"
    );
    for stale in ["--runner", "cloud", "local_inference", "status=Ok"] {
        assert!(
            !stdout.contains(stale),
            "app help must not show stale generic/adapter output `{stale}`:\n{stdout}"
        );
    }
}

#[test]
fn app_run_action_help_renders_manifest_action_usage_without_executing() {
    let output = agh(&["app", "run", "grokrxiv", "review", "--help"]);
    assert!(
        output.status.success(),
        "action help should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: agh app run grokrxiv review"),
        "action help should include concrete usage, got:\n{stdout}"
    );
    for expected in [
        "URL_OR_PATH",
        "--type",
        "--include",
        "--exclude",
        "--with-lean",
        "--no-lean",
        "[conflicts: --no-lean]",
        "[conflicts: --with-lean]",
        "--no-external-actions",
    ] {
        assert!(
            stdout.contains(expected),
            "review help should include `{expected}`, got:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("status=Ok") && !stdout.contains("could not resolve source"),
        "help must not execute the GrokRxiv adapter, got:\n{stdout}"
    );
}

#[test]
fn app_run_rejects_manifest_option_conflicts_before_queueing() {
    let output = agh(&[
        "app",
        "run",
        "grokrxiv",
        "review",
        "2606.24837",
        "--with-lean",
        "--no-lean",
        "--no-external-actions",
    ]);

    assert!(
        !output.status.success(),
        "conflicting manifest options should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("cannot be combined"),
        "expected conflict error, got stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("AgentHero app run") && !stderr.contains("AgentHero app run"),
        "conflict should be rejected before queueing, got stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn app_run_nested_action_help_resolves_manifest_command_path() {
    let output = agh(&["app", "run", "grokrxiv", "validate", "citations", "--help"]);
    assert!(
        output.status.success(),
        "nested action help should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: agh app run grokrxiv validate citations"),
        "nested action help should include concrete usage, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Validate paper citations"),
        "nested action help should use app.yaml description, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("status=Ok"),
        "nested help must not execute the citation DAG, got:\n{stdout}"
    );
}

#[test]
fn app_runs_accepts_state_and_limit_filters() {
    let cli = Cli::try_parse_from([
        "agh",
        "app",
        "runs",
        "--app",
        "grokrxiv",
        "--action",
        "formalize",
        "--state",
        "queued",
        "--limit",
        "10",
    ])
    .expect("app runs filters should parse");

    match cli.command {
        Command::App { command } => match command {
            AppCommand::Runs {
                app,
                action,
                state,
                limit,
            } => {
                assert_eq!(app.as_deref(), Some("grokrxiv"));
                assert_eq!(action.as_deref(), Some("formalize"));
                assert_eq!(state.as_deref(), Some("queued"));
                assert_eq!(limit, 10);
            }
            other => panic!("expected App::Runs command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_cancel_commands_parse_for_operator_cleanup() {
    let run_id = uuid::Uuid::parse_str("68c3a3dd-4ae0-402a-82cc-953153b36702").unwrap();
    let cancel = Cli::try_parse_from([
        "agh",
        "app",
        "cancel",
        &run_id.to_string(),
        "--reason",
        "stale formalize job",
    ])
    .expect("app cancel should parse");

    match cancel.command {
        Command::App { command } => match command {
            AppCommand::Cancel {
                run_id: parsed,
                reason,
            } => {
                assert_eq!(parsed, run_id);
                assert_eq!(reason.as_deref(), Some("stale formalize job"));
            }
            other => panic!("expected App::Cancel command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }

    let bulk = Cli::try_parse_from([
        "agh",
        "app",
        "cancel-queued",
        "--app",
        "grokrxiv",
        "--action",
        "formalize",
        "--except",
        &run_id.to_string(),
        "--older-than-mins",
        "10",
        "--dry-run",
    ])
    .expect("app cancel-queued should parse");

    match bulk.command {
        Command::App { command } => match command {
            AppCommand::CancelQueued {
                app,
                action,
                except,
                older_than_mins,
                dry_run,
                ..
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(action.as_deref(), Some("formalize"));
                assert_eq!(except, vec![run_id]);
                assert_eq!(older_than_mins, Some(10));
                assert!(dry_run);
            }
            other => panic!("expected App::CancelQueued command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_compare_command_parses_for_determinism_audit() {
    let left_run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let right_run_id = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "compare",
        &left_run_id.to_string(),
        &right_run_id.to_string(),
    ])
    .expect("app compare should parse two app-run UUIDs");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Compare {
                left_run_id: left,
                right_run_id: right,
            } => {
                assert_eq!(left, left_run_id);
                assert_eq!(right, right_run_id);
            }
            other => panic!("expected App::Compare command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_replay_command_parses_for_checkpoint_replay() {
    let run_id = uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let parsed = Cli::try_parse_from(["agh", "app", "replay", &run_id.to_string()])
        .expect("app replay should parse one source app-run UUID");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Replay {
                source_run_id: parsed,
            } => {
                assert_eq!(parsed, run_id);
            }
            other => panic!("expected App::Replay command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_work_command_parses_for_one_shot_run_claim() {
    let run_id = uuid::Uuid::parse_str("5ec729c1-9ca6-4535-8a6f-677f91ca05fa").unwrap();
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "work",
        "--run-id",
        &run_id.to_string(),
        "--worker-name",
        "test-worker",
    ])
    .expect("app work should parse");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Work {
                run_id: parsed,
                worker_name,
            } => {
                assert_eq!(parsed, Some(run_id));
                assert_eq!(worker_name.as_deref(), Some("test-worker"));
            }
            other => panic!("expected App::Work command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_run_action_help_ignores_global_log_file_option_and_value() {
    let output = agh(&[
        "app",
        "run",
        "grokrxiv",
        "--log-file",
        ".agenthero/logs/help.jsonl",
        "review",
        "--help",
    ]);

    assert!(
        output.status.success(),
        "app action help should ignore global --log-file and its value, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("agh app run grokrxiv review"));
    assert!(stdout.contains("--with-lean"));
    assert!(
        !stdout.contains("--loop"),
        "deprecated no-op --loop should not be advertised in action help"
    );
}

#[test]
fn app_logs_command_parses_for_operator_log_inspection() {
    let run_id = uuid::Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "logs",
        &run_id.to_string(),
        "--tail",
        "200",
        "--follow",
    ])
    .expect("app logs should parse");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Logs {
                run_id: parsed,
                tail,
                follow,
            } => {
                assert_eq!(parsed, run_id);
                assert_eq!(tail, 200);
                assert!(follow);
            }
            other => panic!("expected App::Logs command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn global_log_file_option_parses_for_structured_platform_audit_logs() {
    let parsed = Cli::try_parse_from([
        "agh",
        "--log-file",
        ".agenthero/logs/agenthero.jsonl",
        "app",
        "list",
    ])
    .expect("global --log-file should parse for platform audit logging");

    assert_eq!(
        parsed.log_file,
        Some(std::path::PathBuf::from(".agenthero/logs/agenthero.jsonl"))
    );
}

#[test]
fn app_events_command_parses_for_operator_event_streaming() {
    let run_id = uuid::Uuid::parse_str("9db4ec15-ae27-4d6c-af58-4d5d10f0a9e4").unwrap();
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "events",
        &run_id.to_string(),
        "--tail",
        "75",
        "--follow",
    ])
    .expect("app events should parse");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Events {
                run_id: parsed,
                tail,
                follow,
            } => {
                assert_eq!(parsed, run_id);
                assert_eq!(tail, 75);
                assert!(follow);
            }
            other => panic!("expected App::Events command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_approve_run_command_parses_for_paused_dag_resume() {
    let run_id = uuid::Uuid::parse_str("95064566-4c4d-4a38-9378-e00f88f504af").unwrap();
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "approve-run",
        &run_id.to_string(),
        "--key",
        "approval/human_release",
    ])
    .expect("app approve-run should parse");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::ApproveRun {
                run_id: parsed,
                key,
            } => {
                assert_eq!(parsed, run_id);
                assert_eq!(key, "approval/human_release");
            }
            other => panic!("expected App::ApproveRun command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[test]
fn app_enqueue_command_parses_for_tracked_app_run() {
    let parsed = Cli::try_parse_from([
        "agh",
        "app",
        "enqueue",
        "grokrxiv",
        "formalize",
        "fb7eaf59-ec86-4240-93b5-1ef32f57b3a4",
        "--debug",
    ])
    .expect("app enqueue should parse");

    match parsed.command {
        Command::App { command } => match command {
            AppCommand::Enqueue { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "formalize",
                        "fb7eaf59-ec86-4240-93b5-1ef32f57b3a4",
                        "--debug"
                    ]
                );
            }
            other => panic!("expected App::Enqueue command, got {other:?}"),
        },
        other => panic!("expected App command, got {other:?}"),
    }
}

#[tokio::test]
async fn app_run_http_write_route_requires_service_token() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::post("/apps/grokrxiv/actions/validate-citations/runs")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn app_run_events_http_route_accepts_cursor_query() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::get(
                "/app-runs/11111111-1111-1111-1111-111111111111/events?after_id=10&limit=25",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn app_run_logs_http_route_is_file_backed_and_db_free() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::get(
                "/app-runs/11111111-1111-1111-1111-111111111111/logs?tail=25&max_bytes=64",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).expect("logs route response JSON");

    assert_eq!(value["run_id"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(value["exists"], false);
    assert_eq!(value["tail"], "");
    assert_eq!(value["tail_lines"], 25);
    assert_eq!(value["max_bytes"], 64);
    assert_eq!(
        value["log_contract"]["format"],
        "durable_text_log_with_agenthero_event_jsonl"
    );
    assert_eq!(value["log_contract"]["tail_parameter"], "tail");
    assert_eq!(value["log_contract"]["max_bytes_parameter"], "max_bytes");
    assert_eq!(value["log_contract"]["trace_fields"][0], "app_run_id");
    assert!(
        value["log_path"]
            .as_str()
            .expect("log path")
            .ends_with("11111111-1111-1111-1111-111111111111.log"),
        "log route should expose the durable app-run log path"
    );
}

#[tokio::test]
async fn metrics_route_exposes_platform_observability_without_database() {
    let response = agenthero_orchestrator::router()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).expect("metrics are utf8");

    assert!(text.contains("# HELP agenthero_database_configured"));
    assert!(text.contains("agenthero_database_configured 0"));
    assert!(text.contains("agenthero_database_query_ok 0"));
    assert!(text.contains("agenthero_app_runs{state=\"queued\"} 0"));
    assert!(text.contains("agenthero_app_runs{state=\"running\"} 0"));
    assert!(text.contains("# HELP agenthero_app_runs_by_action"));
    assert!(text.contains("# TYPE agenthero_app_runs_by_action gauge"));
    assert!(text.contains("agenthero_dag_runs{state=\"queued\"} 0"));
    assert!(text.contains("agenthero_dag_runs{state=\"running\"} 0"));
    assert!(text.contains("agenthero_dag_runs{state=\"awaiting_approval\"} 0"));
    assert!(text.contains("# HELP agenthero_dag_runs_by_app_dag"));
    assert!(text.contains("# TYPE agenthero_dag_runs_by_app_dag gauge"));
    assert!(text.contains("agenthero_worker_leases{state=\"leased\"} 0"));
    assert!(text.contains("agenthero_dag_run_nodes{state=\"failed\"} 0"));
    assert!(text.contains("# HELP agenthero_dag_run_nodes_by_node"));
    assert!(text.contains("# TYPE agenthero_dag_run_nodes_by_node gauge"));
    assert!(text.contains("agenthero_dag_run_node_latency_ms_count 0"));
    assert!(text.contains("agenthero_dag_run_node_latency_ms_sum 0"));
    assert!(text.contains("# HELP agenthero_dag_run_node_latency_ms_by_node_count"));
    assert!(text.contains("# TYPE agenthero_dag_run_node_latency_ms_by_node_count gauge"));
    assert!(text.contains("# HELP agenthero_dag_run_node_latency_ms_by_node_sum"));
    assert!(text.contains("# TYPE agenthero_dag_run_node_latency_ms_by_node_sum gauge"));
    assert!(text.contains("# HELP agenthero_dag_node_retries_by_node"));
    assert!(text.contains("# TYPE agenthero_dag_node_retries_by_node gauge"));
    assert!(text.contains("agenthero_dag_events{event_type=\"node.retry_scheduled\"} 0"));
    assert!(text.contains("agenthero_dag_events{event_type=\"node.failed\"} 0"));
    assert!(text.contains("agenthero_dag_events{event_type=\"app_run.lease_expired_requeued\"} 0"));
    assert!(text.contains("# HELP agenthero_dag_events_by_app_dag"));
    assert!(text.contains("# TYPE agenthero_dag_events_by_app_dag gauge"));
    assert!(text.contains("# HELP agenthero_dag_events_by_app_action"));
    assert!(text.contains("# TYPE agenthero_dag_events_by_app_action gauge"));
    assert!(text.contains("# HELP agenthero_dag_artifact_bytes_by_app_dag"));
    assert!(text.contains("# TYPE agenthero_dag_artifact_bytes_by_app_dag gauge"));
}

#[tokio::test]
async fn app_runs_index_reports_observable_list_contract_without_database() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::get("/app-runs?limit=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).expect("response is json");

    assert_eq!(value["error"], "database_unconfigured");
    assert_eq!(value["list_contract"]["item"], "AppRunListItem");
    assert_eq!(
        value["list_contract"]["observability"]["event_count"],
        "durable dag_events rows for this app run"
    );
    assert_eq!(
        value["list_contract"]["observability"]["links"]["event_stream_path"],
        "/app-runs/<run_id>/events/stream"
    );
}

#[tokio::test]
async fn app_run_events_http_route_advertises_trace_field_contract() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::get("/app-runs/11111111-1111-1111-1111-111111111111/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).expect("events route response JSON");

    assert_eq!(value["event_contract"]["trace_fields"][0], "app_run_id");
    assert!(
        value["event_contract"]["trace_fields"]
            .as_array()
            .expect("trace fields")
            .iter()
            .any(|field| field == "duration_ms"),
        "event stream contract should advertise duration_ms"
    );
}

#[tokio::test]
async fn app_run_events_stream_http_route_is_sse_monitor_surface_without_database() {
    let response = agenthero_orchestrator::router()
        .oneshot(
            Request::get(
                "/app-runs/11111111-1111-1111-1111-111111111111/events/stream?after_id=10&limit=25",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).expect("sse stream is utf8");

    assert!(content_type.starts_with("text/event-stream"));
    assert!(text.contains("event: agenthero.error"));
    assert!(text.contains("data: "));
    assert!(text.contains("\"event_contract\""));
    assert!(text.contains("\"trace_fields\""));
    assert!(text.contains("\"app_run_id\""));
}

#[tokio::test]
async fn app_run_http_write_route_requires_authorization_when_configured() {
    let router = agenthero_orchestrator::router_with_state(agenthero_orchestrator::PlatformState {
        pool: None,
        service_token: Some("secret".to_string()),
    });

    let response = router
        .oneshot(
            Request::post("/apps/grokrxiv/actions/validate-citations/runs")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn app_run_http_write_route_reports_missing_database_after_auth() {
    let router = agenthero_orchestrator::router_with_state(agenthero_orchestrator::PlatformState {
        pool: None,
        service_token: Some("secret".to_string()),
    });

    let response = router
        .oneshot(
            Request::post("/apps/grokrxiv/actions/validate-citations/runs")
                .header("content-type", "application/json")
                .header("authorization", "Bearer secret")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn app_show_http_route_is_read_only_and_db_free() {
    let response = agenthero_orchestrator::router()
        .oneshot(Request::get("/apps/grokrxiv").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn app_action_catalog_exposes_options_from_yaml() {
    let grokrxiv = agenthero_orchestrator::dag_apps::load_app_manifest_by_slug("grokrxiv")
        .expect("load grokrxiv app manifest");
    let review = grokrxiv
        .actions
        .iter()
        .find(|action| action.id == "review")
        .expect("review action");
    assert!(
        review.options.iter().any(|option| option.name == "source"
            && option.kind == "positional"
            && option.required),
        "review action must document its required source argument"
    );
    assert!(
        review
            .options
            .iter()
            .any(|option| option.name == "--include" && option.multiple),
        "repeatable review flags should be visible in the app catalog"
    );

    let c2rust = agenthero_orchestrator::dag_apps::load_app_manifest_by_slug("c2rust")
        .expect("load c2rust app manifest");
    let migrate = c2rust
        .actions
        .iter()
        .find(|action| action.id == "migrate")
        .expect("migrate action");
    assert!(
        migrate
            .options
            .iter()
            .any(|option| option.name == "source" && option.required),
        "new DAGOps apps should expose their own app-level argument contract"
    );
}

#[test]
fn app_manifests_are_yaml_source_of_truth() {
    let root = workspace_root();
    let app_dir = root.join("agenthero").join("apps");
    let expected = [
        ("grokrxiv", "GrokRxiv"),
        ("c2rust", "C2Rust"),
        ("platform-smoke", "Platform Smoke"),
    ];

    for (slug, label) in expected {
        let path = app_dir.join(slug).join("app.yaml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        let manifest: Value = serde_yaml::from_str(&text)
            .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
        assert_eq!(manifest["slug"], slug);
        assert_eq!(manifest["label"], label);
        assert!(
            manifest["actions"]
                .as_array()
                .is_some_and(|actions| !actions.is_empty()),
            "{} must declare app actions",
            path.display()
        );
    }
}

#[test]
fn every_grokrxiv_manifest_action_has_parseable_cli_shape() {
    let manifest = agenthero_orchestrator::dag_apps::load_app_manifest_by_slug("grokrxiv")
        .expect("load grokrxiv app manifest");
    let review_id = "03c0843f-80f8-46b4-8d7a-ad7292c449f8";

    for action in manifest.actions {
        let mut argv = vec![
            "agh".to_string(),
            "app".to_string(),
            "run".to_string(),
            "grokrxiv".to_string(),
        ];
        argv.extend(action.command.iter().cloned());
        argv.extend(sample_grokrxiv_args(&action.id, review_id));

        let cli = Cli::try_parse_from(argv.iter().map(String::as_str))
            .unwrap_or_else(|err| panic!("{} should parse: {err}", action.id));
        match cli.command {
            Command::App { command } => match command {
                AppCommand::Run { app, args } => {
                    assert_eq!(app, "grokrxiv");
                    assert!(
                        args.starts_with(&action.command),
                        "{} should preserve command path {:?}, got {:?}",
                        action.id,
                        action.command,
                        args
                    );
                }
                other => panic!("{} parsed to wrong app command: {other:?}", action.id),
            },
            other => panic!("{} parsed to wrong command: {other:?}", action.id),
        }

        agenthero_orchestrator::dag_apps::validate_app_action_args(
            &action,
            &sample_grokrxiv_args(&action.id, review_id),
        )
        .unwrap_or_else(|err| panic!("{} manifest args should validate: {err:#}", action.id));
    }
}

#[test]
fn every_grokrxiv_manifest_action_flag_validates_with_sample_value() {
    let manifest = agenthero_orchestrator::dag_apps::load_app_manifest_by_slug("grokrxiv")
        .expect("load grokrxiv app manifest");
    let review_id = "03c0843f-80f8-46b4-8d7a-ad7292c449f8";

    for action in manifest.actions {
        for option in action
            .options
            .iter()
            .filter(|option| option.kind != "positional")
        {
            let mut args = sample_grokrxiv_args(&action.id, review_id);
            if !args.iter().any(|arg| arg == &option.name) {
                args.push(option.name.clone());
                if let Some(value_name) = option.value_name.as_deref() {
                    args.push(sample_grokrxiv_option_value(value_name, review_id));
                }
            }

            agenthero_orchestrator::dag_apps::validate_app_action_args(&action, &args)
                .unwrap_or_else(|err| {
                    panic!(
                        "{} option {} should validate with args {:?}: {err:#}",
                        action.id, option.name, args
                    )
                });
        }
    }
}

fn sample_grokrxiv_args(action: &str, review_id: &str) -> Vec<String> {
    match action {
        "extract" | "ingest" | "review" | "review-extracted" => vec!["2605.17307".into()],
        "ingest-range" => vec![
            "--from".into(),
            "2026-05-01".into(),
            "--to".into(),
            "2026-05-02".into(),
        ],
        "ingest-daily" | "batch-list" | "validate-citations" => vec![],
        "re-review"
        | "formalize"
        | "verify"
        | "render"
        | "refresh-review"
        | "show"
        | "open"
        | "approve"
        | "html-review"
        | "feedback-loop-smoke"
        | "batch-status"
        | "batch-run" => vec![review_id.into()],
        "request-revisions" => vec![review_id.into(), "--notes".into(), "Needs revision.".into()],
        "request-changes" => vec![review_id.into(), "--notes".into(), "Needs changes.".into()],
        "reject" => vec![review_id.into(), "--reason".into(), "Out of scope.".into()],
        "close" => vec![
            review_id.into(),
            "--reason".into(),
            "Closed by operator.".into(),
        ],
        "withdraw" => vec![
            review_id.into(),
            "--reason".into(),
            "Withdrawn by operator.".into(),
        ],
        "correct" => vec![
            review_id.into(),
            "--rationale-md".into(),
            "corrections/reason.md".into(),
        ],
        "list" => vec!["reviews".into()],
        "batch-create" => vec![
            "--category".into(),
            "math".into(),
            "--month".into(),
            "2026-05".into(),
            "--daily-limit".into(),
            "1".into(),
        ],
        other => panic!("missing sample args for GrokRxiv action `{other}`"),
    }
}

fn sample_grokrxiv_option_value(value_name: &str, review_id: &str) -> String {
    match value_name {
        "YYYY-MM-DD" => "2026-05-02".into(),
        "YYYY-MM" => "2026-05".into(),
        "N" => "1".into(),
        "SECONDS" => "5".into(),
        "STATUS" => "queued".into(),
        "PATH" => "path/to/artifact".into(),
        "UUID" => review_id.into(),
        "TEXT" => "Operator note.".into(),
        "TITLE" => "Sample title".into(),
        "FIELD" => "math".into(),
        "CLAIM_ID" => "claim:sample".into(),
        "GLOB" => "**/*.tex".into(),
        "CSV" => "math,cs".into(),
        "REF" => "main".into(),
        "ROLE=PROVIDER" => "citation=claude".into(),
        "ROLE=MODEL" => "citation=sonnet[1m]".into(),
        "ROLE=RUNNER" => "citation=cli".into(),
        "ARXIV_SET" => "math".into(),
        "arxiv|pdf|tex|git|mixed" => "arxiv".into(),
        "html|md|tex|pdf|zip" => "md".into(),
        "all|inventory|packet|harness|author|lean-check|fix|faithfulness" => "all".into(),
        other => panic!("missing sample value for GrokRxiv option value `{other}`"),
    }
}
