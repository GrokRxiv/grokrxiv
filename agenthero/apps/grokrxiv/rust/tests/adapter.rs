use std::process::{Command, Stdio};

use agenthero_agent_runtime::{
    AppAdapterRequest, AGENTHERO_EVENT_TRACE_FIELDS, APP_ADAPTER_EVENT_PREFIX,
};
use agenthero_dag_executor::DagIo;
use serde_json::json;

#[test]
fn grokrxiv_review_adapter_emits_app_lifecycle_events_for_manifest_dag() {
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
        "grokrxiv",
        "review",
        "review-loop",
        vec![
            "2606.24837".to_string(),
            "--type".to_string(),
            "arxiv".to_string(),
            "--no-lean".to_string(),
            "--no-external-actions".to_string(),
        ],
        input,
        true,
        true,
    )
    .with_idempotency_key("app-run:2d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-grokrxiv"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn grokrxiv adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
        .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("adapter response is JSON");
    assert_eq!(response["ok"], true);
    assert_eq!(response["report"]["status"], "ok");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let started = lifecycle_event(&stderr, "app_action.started");
    let completed = lifecycle_event(&stderr, "app_action.completed");
    assert_lifecycle_trace_fields(&started);
    assert_lifecycle_trace_fields(&completed);
    assert_eq!(
        started["payload"]["app_run_id"],
        "2d0a1d88-b9f9-4e8f-848e-605b86717330"
    );
    assert_eq!(
        completed["payload"]["dag_run_id"],
        "f78c57db-89e3-4b63-8c1a-2c07e3331f0c"
    );
    assert_eq!(
        completed["payload"]["lease_id"],
        "a9353847-48b3-472e-b88e-89770fcdbf7a"
    );
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
