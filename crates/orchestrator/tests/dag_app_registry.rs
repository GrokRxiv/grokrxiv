use agenthero_dag_runtime::{DagManifest, DagNodeStatus};

#[test]
fn app_registry_groups_dag_types_behind_product_apps() {
    let ids = agenthero_orchestrator::dag_apps::registered_app_ids();
    assert_eq!(ids, vec!["c2rust".to_string(), "grokrxiv".to_string()]);

    let grokrxiv = agenthero_orchestrator::dag_apps::registered_app("grokrxiv")
        .expect("GrokRxiv app descriptor");
    let grokrxiv_actions = grokrxiv
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    for action in [
        "extract",
        "review",
        "review-extracted",
        "show",
        "list",
        "open",
        "approve",
        "request-revisions",
        "request-changes",
        "reject",
    ] {
        assert!(
            grokrxiv_actions.contains(&action),
            "GrokRxiv app must expose `{action}`"
        );
    }

    let c2rust =
        agenthero_orchestrator::dag_apps::registered_app("c2rust").expect("c2rust app descriptor");
    assert_eq!(
        c2rust
            .actions
            .iter()
            .map(|action| action.dag_type.as_str())
            .collect::<Vec<_>>(),
        vec!["c2rust"]
    );
}

#[test]
fn registry_contains_grokrxiv_chain_and_c2rust_apps() {
    let ids = agenthero_orchestrator::dag_apps::registered_dag_app_ids();

    assert_eq!(
        ids,
        vec![
            "c2rust".to_string(),
            "citation-validation".to_string(),
            "paper-extract".to_string(),
            "paper-ingest".to_string(),
            "paper-publish".to_string(),
            "paper-review".to_string(),
            "paper-revise".to_string(),
        ]
    );
}

#[test]
fn orchestrator_does_not_depend_on_dag_app_crates() {
    let manifest = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read orchestrator Cargo.toml");

    for forbidden in [
        "agenthero-dag-app-c2rust",
        "agenthero-dag-app-grokrxiv",
        "grokrxiv-app-runtime",
        "grokrxiv-extraction",
        "grokrxiv-ingest",
        "grokrxiv-publisher",
        "grokrxiv-render",
        "grokrxiv-schemas",
        "grokrxiv-storage",
        "grokrxiv-verifier",
        "grokrxiv-dag-app-citation-validation",
        "grokrxiv-dag-app-paper-extract",
        "grokrxiv-dag-app-paper-ingest",
        "grokrxiv-dag-app-paper-publish",
        "grokrxiv-dag-app-paper-review",
        "grokrxiv-dag-app-paper-revise",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "orchestrator must not depend on app crate `{forbidden}`; app manifests declare adapters"
        );
    }
}

#[test]
fn root_workspace_only_contains_platform_crates() {
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let parsed: toml::Value = toml::from_str(&manifest).expect("parse root Cargo.toml");
    let members = parsed["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|value| value.as_str().expect("member string"))
        .collect::<Vec<_>>();

    assert_eq!(
        members,
        vec![
            "crates/dag-runtime",
            "crates/dag-executor",
            "crates/agent-runtime",
            "crates/orchestrator",
            "crates/llm-adapter",
        ]
    );
    assert!(
        members.iter().all(|member| !member.starts_with("agenthero/apps/")),
        "apps must build from their own manifests, not root workspace membership"
    );
}

#[test]
fn top_level_crates_directory_is_platform_only() {
    let root = workspace_root();
    let mut crates = std::fs::read_dir(root.join("crates"))
        .expect("read crates/")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.is_dir() {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    crates.sort();
    assert_eq!(
        crates,
        vec![
            "agent-runtime",
            "dag-executor",
            "dag-runtime",
            "llm-adapter",
            "orchestrator",
        ]
    );
}

#[test]
fn orchestrator_source_has_no_grokrxiv_domain_code() {
    let root = workspace_root();
    let src = root.join("crates").join("orchestrator").join("src");
    let forbidden = [
        "grokrxiv",
        "GrokRxiv",
        "arxiv",
        "paper-review",
        "paper_extract",
        "paper-extract",
    ];
    for path in walk_rs_files(&src) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{} must not contain app-specific token `{needle}`",
                path.display()
            );
        }
    }
}

#[test]
fn app_manifest_resolves_action_command_paths() {
    let review = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["review".into(), "2605.17307".into()],
    )
    .expect("review action resolves");
    assert_eq!(review.id, "review");
    assert_eq!(review.dag_type, "paper-review");
    assert_eq!(review.args, vec!["2605.17307"]);

    let citations = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["validate".into(), "citations".into()],
    )
    .expect("nested validate citations action resolves");
    assert_eq!(citations.id, "validate-citations");
    assert_eq!(citations.dag_type, "citation-validation");
    assert!(citations.args.is_empty());

    let err = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["validate".into(), "metadata".into()],
    )
    .expect_err("unknown nested app action must fail");
    assert!(err.to_string().contains("unknown app action"));
}

#[test]
fn every_registered_app_has_a_valid_manifest() {
    for app in agenthero_orchestrator::dag_apps::registered_dag_apps()
        .expect("registered DAG apps load")
    {
        let path = app.manifest_path;
        let manifest = DagManifest::from_path(&path)
            .unwrap_or_else(|err| panic!("{} should be valid: {err}", path.display()));
        assert_eq!(manifest.id.as_str(), app.dag_type);
    }
}

#[test]
fn app_contracts_are_owned_by_app_roots() {
    let root = workspace_root();

    for app in ["grokrxiv", "c2rust"] {
        let app_root = root.join("agenthero").join("apps").join(app);
        assert!(
            app_root.join("app.yaml").is_file(),
            "{} must declare its product app manifest inside the app root",
            app_root.display()
        );
        assert!(
            app_root.join("dags").is_dir(),
            "{} must own its DAG manifests",
            app_root.display()
        );
    }

    for legacy_root in ["dags", "agents", "prompts"] {
        assert!(
            !root.join(legacy_root).exists(),
            "legacy root-level `{legacy_root}/` must not remain an app contract source"
        );
    }
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn walk_rs_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)
            .unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
        {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

#[tokio::test]
async fn registry_runs_c2rust_manifest_through_declared_adapter() {
    let report =
        agenthero_orchestrator::dag_apps::run_registered_dag_app(
            "c2rust",
            agenthero_dag_executor::DagIo::default(),
        )
            .await
            .expect("c2rust run");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
}
