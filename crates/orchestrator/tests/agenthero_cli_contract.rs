use clap::{CommandFactory, Parser};
use serde_json::Value;

use agenthero_orchestrator::cli::{AppCommand, Cli, Command};

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
