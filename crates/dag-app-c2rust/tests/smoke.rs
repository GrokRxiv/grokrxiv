use std::path::PathBuf;

use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagApp, DagExecutor, DagIo};
use agenthero_dag_runtime::DagManifest;

#[tokio::test]
async fn c2rust_runs_through_generic_executor_without_paper_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf();
    let manifest = DagManifest::from_path(root.join("dags/c2rust.yaml")).expect("manifest");
    let app = C2RustDagApp::default();

    assert_eq!(app.dag_type(), "c2rust");
    assert_eq!(app.manifest_file(), "c2rust.yaml");

    let report = DagExecutor::new(app)
        .execute(&manifest, DagIo::default())
        .await
        .expect("c2rust executor smoke");

    assert_eq!(report.dag_type.as_str(), "c2rust");
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
    assert_eq!(
        report.outputs.values["lint_pass"]["dag_type"],
        serde_json::json!("c2rust")
    );
}
