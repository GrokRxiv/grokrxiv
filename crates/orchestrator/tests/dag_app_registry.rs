use agenthero_dag_executor::DagIo;
use agenthero_dag_runtime::{DagManifest, DagNodeStatus};

#[test]
fn app_registry_groups_dag_types_behind_product_apps() {
    let ids = agenthero_orchestrator::dag_apps::registered_app_ids();
    assert_eq!(ids, vec!["c2rust", "grokrxiv"]);

    let grokrxiv = agenthero_orchestrator::dag_apps::registered_app("grokrxiv")
        .expect("GrokRxiv app descriptor");
    let grokrxiv_actions = grokrxiv
        .actions
        .iter()
        .map(|action| action.id)
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
            .map(|action| action.dag_type)
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
            "c2rust",
            "citation-validation",
            "paper-extract",
            "paper-ingest",
            "paper-publish",
            "paper-review",
            "paper-revise",
        ]
    );
}

#[test]
fn every_registered_app_has_a_valid_manifest() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf();

    for app in agenthero_orchestrator::dag_apps::registered_dag_apps() {
        let path = root.join("dags").join(app.manifest_file);
        let manifest = DagManifest::from_path(&path)
            .unwrap_or_else(|err| panic!("{} should be valid: {err}", path.display()));
        assert_eq!(manifest.id.as_str(), app.dag_type);
    }
}

#[tokio::test]
async fn registry_runs_c2rust_manifest_through_generic_executor() {
    let report =
        agenthero_orchestrator::dag_apps::run_registered_dag_app("c2rust", DagIo::default())
            .await
            .expect("c2rust run");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
}
