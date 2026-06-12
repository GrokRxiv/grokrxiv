use axum::{
    body::Body,
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
fn app_run_grokrxiv_review_loop_parses_as_product_path() {
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
    ])
    .expect("review --loop should parse as the canonical app review path");

    assert!(cli.json);
    assert!(cli.dry_run);
    match cli.command {
        Command::App { command } => match command {
            AppCommand::Run { app, args } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec!["review", "https://arxiv.org/abs/2606.00799", "--loop"]
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
        "--loop",
    ])
    .expect("review --loop should parse");
    assert!(agenthero_orchestrator::cli::stream_app_stderr_for_cli(&cli));

    let quiet = Cli::try_parse_from([
        "agh",
        "--no-status",
        "app",
        "run",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2606.00799",
        "--loop",
    ])
    .expect("review --loop --no-status should parse");
    assert!(!agenthero_orchestrator::cli::stream_app_stderr_for_cli(
        &quiet
    ));
}

#[test]
fn grokrxiv_review_action_catalog_declares_loop_debug_options_and_dag() {
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
        "review --loop should be the product review path, with paper-review called inside the loop DAG"
    );
    let options = review
        .get("options")
        .and_then(|value| value.as_sequence())
        .expect("review options");
    let loop_option = options
        .iter()
        .find(|option| option.get("name").and_then(|name| name.as_str()) == Some("--loop"))
        .expect("review action should advertise --loop");
    assert_eq!(
        loop_option.get("kind").and_then(|value| value.as_str()),
        Some("flag")
    );
    let debug_option = options
        .iter()
        .find(|option| option.get("name").and_then(|name| name.as_str()) == Some("--debug"))
        .expect("review action should advertise --debug");
    assert_eq!(
        debug_option.get("kind").and_then(|value| value.as_str()),
        Some("flag")
    );
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
        "knowledge_graph_builder",
        "semantic_category_mapper",
        "haskell_review_fix_code",
        "proof_obligation_generator",
        "lean_review_fix_code",
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
        "haskell_semantic_author",
        "haskell_code_reviewer",
        "haskell_code_fixer",
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

    for loop_id in [
        "haskell_review_fix_code",
        "lean_review_fix_code",
        "pr_review_fix_code",
    ] {
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
    for expected in ["URL_OR_PATH", "--type", "--include", "--exclude"] {
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
        "agh", "app", "runs", "--app", "grokrxiv", "--state", "queued", "--limit", "10",
    ])
    .expect("app runs filters should parse");

    match cli.command {
        Command::App { command } => match command {
            AppCommand::Runs { app, state, limit } => {
                assert_eq!(app.as_deref(), Some("grokrxiv"));
                assert_eq!(state.as_deref(), Some("queued"));
                assert_eq!(limit, 10);
            }
            other => panic!("expected App::Runs command, got {other:?}"),
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
    let expected = [("grokrxiv", "GrokRxiv"), ("c2rust", "C2Rust")];

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
        | "verify"
        | "render"
        | "refresh-review"
        | "show"
        | "open"
        | "approve"
        | "request-changes"
        | "close"
        | "withdraw"
        | "correct"
        | "html-review"
        | "feedback-loop-smoke"
        | "batch-status"
        | "batch-run" => vec![review_id.into()],
        "request-revisions" => vec![review_id.into(), "--notes".into(), "Needs revision.".into()],
        "reject" => vec![review_id.into(), "--reason".into(), "Out of scope.".into()],
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
