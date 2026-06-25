use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use agenthero_agent_runtime::{
    AppAdapterRequest, AGENTHERO_EVENT_TRACE_FIELDS, APP_ADAPTER_EVENT_PREFIX,
};
use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagApp, DagExecutor, DagIo};
use agenthero_dag_runtime::DagManifest;
use serde_json::json;

#[tokio::test]
async fn c2rust_runs_through_generic_executor_without_paper_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("app root")
        .to_path_buf();
    let manifest = DagManifest::from_path(root.join("dags/c2rust.yaml")).expect("manifest");
    let workspace = temp_workspace("c2rust-smoke");
    let app = C2RustDagApp::with_artifact_root(&workspace);

    assert_eq!(app.dag_type(), "c2rust");
    assert_eq!(app.manifest_file(), "c2rust.yaml");

    let mut input = DagIo::default();
    input
        .values
        .insert("source".to_string(), json!("/tmp/input.c"));

    let report = DagExecutor::new(app)
        .execute(&manifest, input)
        .await
        .expect("c2rust executor smoke");

    assert_eq!(report.dag_type.as_str(), "c2rust");
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
    assert_eq!(
        report.outputs.values["lint_pass"]["dag_type"],
        serde_json::json!("c2rust")
    );
    assert!(report
        .outputs
        .artifacts
        .contains_key("migration_report.md"));
    assert!(report
        .outputs
        .artifacts
        .contains_key("migration/translated.rs"));
    let report_path = &report.outputs.artifacts["migration_report.md"].uri;
    let report_body = std::fs::read_to_string(report_path).expect("migration report written");
    assert!(report_body.contains("Source: `/tmp/input.c`"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[test]
fn c2rust_adapter_emits_app_lifecycle_events() {
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("a9353847-48b3-472e-b88e-89770fcdbf7a"),
    );
    let request = AppAdapterRequest::new(
        "c2rust",
        "migrate",
        "c2rust",
        vec!["/tmp/input.c".to_string()],
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:2d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-c2rust"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn c2rust adapter");
    serde_json::to_writer(
        child.stdin.as_mut().expect("adapter stdin"),
        &request,
    )
    .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let started = lifecycle_event(&stderr, "app_action.started");
    let completed = lifecycle_event(&stderr, "app_action.completed");
    assert_lifecycle_trace_fields(&started);
    assert_lifecycle_trace_fields(&completed);
    assert_eq!(
        completed["payload"]["app_run_id"],
        "2d0a1d88-b9f9-4e8f-848e-605b86717330"
    );
}

fn temp_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("agenthero-{label}-{}-{nanos}", std::process::id()))
}

fn lifecycle_event(stderr: &str, event_type: &str) -> serde_json::Value {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(APP_ADAPTER_EVENT_PREFIX))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event_type"] == event_type)
        .unwrap_or_else(|| panic!("missing lifecycle event `{event_type}` in stderr:\n{stderr}"))
}

fn assert_lifecycle_trace_fields(event: &serde_json::Value) {
    for field in AGENTHERO_EVENT_TRACE_FIELDS {
        assert!(
            event["payload"].get(*field).is_some(),
            "lifecycle event `{}` missing mandatory trace field `{field}`",
            event["event_type"]
        );
    }
}
