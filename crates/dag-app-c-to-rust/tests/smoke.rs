use std::path::PathBuf;

use grokrxiv_dag_app_c_to_rust::CToRustDagApp;
use grokrxiv_dag_executor::{DagApp, DagExecutor, DagIo};
use grokrxiv_dag_runtime::DagManifest;

#[tokio::test]
async fn c_to_rust_runs_through_generic_executor_without_paper_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf();
    let manifest = DagManifest::from_path(root.join("dags/c-to-rust.yaml")).expect("manifest");
    let app = CToRustDagApp::default();

    assert_eq!(app.dag_type(), "c-to-rust");
    assert_eq!(app.manifest_file(), "c-to-rust.yaml");

    let report = DagExecutor::new(app)
        .execute(&manifest, DagIo::default())
        .await
        .expect("c-to-rust executor smoke");

    assert_eq!(report.dag_type.as_str(), "c-to-rust");
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
    assert_eq!(
        report.outputs.values["lint_pass"]["dag_type"],
        serde_json::json!("c-to-rust")
    );
}
