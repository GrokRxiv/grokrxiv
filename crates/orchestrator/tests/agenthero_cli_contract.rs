use clap::{CommandFactory, Parser};
use serde_json::Value;

use agenthero_orchestrator::cli::{Cli, Command};

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
fn direct_grokrxiv_app_command_parses_without_legacy_app_run() {
    let cli = Cli::try_parse_from([
        "agh",
        "grokrxiv",
        "review",
        "https://arxiv.org/abs/2509.09915v1",
    ])
    .expect("direct grokrxiv review command should parse");

    match cli.command {
        Command::GrokRxiv { args } => {
            assert_eq!(args, vec!["review", "https://arxiv.org/abs/2509.09915v1"]);
        }
        other => panic!("expected GrokRxiv command, got {other:?}"),
    }
}

#[test]
fn direct_nested_grokrxiv_validation_command_parses() {
    let cli = Cli::try_parse_from(["agh", "--json", "grokrxiv", "validate", "citations"])
        .expect("direct grokrxiv validate citations command should parse");

    assert!(cli.json);
    match cli.command {
        Command::GrokRxiv { args } => assert_eq!(args, vec!["validate", "citations"]),
        other => panic!("expected GrokRxiv command, got {other:?}"),
    }
}

#[test]
fn direct_c2rust_command_parses_and_legacy_c_to_rust_does_not() {
    let cli = Cli::try_parse_from(["agh", "c2rust", "migrate", "fixtures/kernel.c"])
        .expect("direct c2rust migrate command should parse");
    match cli.command {
        Command::C2Rust { args } => assert_eq!(args, vec!["migrate", "fixtures/kernel.c"]),
        other => panic!("expected C2Rust command, got {other:?}"),
    }

    assert!(
        Cli::try_parse_from(["agh", "app", "run", "c2rust", "migrate"]).is_err(),
        "legacy app/run c2rust path must not remain callable"
    );
}

#[test]
fn app_manifests_are_yaml_source_of_truth() {
    let root = workspace_root();
    let app_dir = root.join("agenthero").join("apps");
    let expected = [("grokrxiv", "GrokRxiv"), ("c2rust", "C2Rust")];

    for (slug, label) in expected {
        let path = app_dir.join(format!("{slug}.yaml"));
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
        let mut argv = vec!["agh".to_string(), "grokrxiv".to_string()];
        argv.extend(action.command.iter().cloned());
        argv.extend(sample_grokrxiv_args(&action.id, review_id));

        let cli = Cli::try_parse_from(argv.iter().map(String::as_str))
            .unwrap_or_else(|err| panic!("{} should parse: {err}", action.id));
        match cli.command {
            Command::GrokRxiv { args } => {
                assert!(
                    args.starts_with(&action.command),
                    "{} should preserve command path {:?}, got {:?}",
                    action.id,
                    action.command,
                    args
                );
            }
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
