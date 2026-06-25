use std::process::{Command, Stdio};

use agenthero_agent_runtime::{
    AppAdapterRequest, AGENTHERO_EVENT_TRACE_FIELDS, APP_ADAPTER_EVENT_PREFIX,
};
use agenthero_dag_executor::DagIo;
use serde_json::json;

#[test]
fn platform_smoke_adapter_emits_app_lifecycle_events() {
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
        "platform-smoke",
        "tool-policy-smoke",
        "tool-policy-smoke",
        Vec::new(),
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:2d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-platform-smoke"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn platform-smoke adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
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

#[test]
fn platform_smoke_adapter_emits_failed_lifecycle_for_failed_dag_report() {
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("3d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("a78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("b9353847-48b3-472e-b88e-89770fcdbf7a"),
    );
    let request = AppAdapterRequest::new(
        "platform-smoke",
        "policy-denial-smoke",
        "policy-denial-smoke",
        Vec::new(),
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:3d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-platform-smoke"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn platform-smoke adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
        .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should serialize failed response instead of crashing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("adapter response is JSON");
    assert_eq!(response["ok"], false);
    assert_eq!(response["report"]["status"], "failed");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("policy requires isolated runner")),
        "response should carry policy denial error: {response:#}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let failed = lifecycle_event(&stderr, "app_action.failed");
    assert_lifecycle_trace_fields(&failed);
    assert_no_lifecycle_event(&stderr, "app_action.completed");
    assert_eq!(
        failed["payload"]["app_run_id"],
        "3d0a1d88-b9f9-4e8f-848e-605b86717330"
    );
}

#[test]
fn platform_smoke_adapter_reports_failed_lifecycle_for_isolation_boundary_dag() {
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("5d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("c78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("d9353847-48b3-472e-b88e-89770fcdbf7a"),
    );
    let request = AppAdapterRequest::new(
        "platform-smoke",
        "isolation-boundary-smoke",
        "isolation-boundary-smoke",
        Vec::new(),
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:5d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-platform-smoke"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn platform-smoke adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
        .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should serialize failed response instead of crashing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("adapter response is JSON");
    assert_eq!(response["ok"], false);
    assert_eq!(response["report"]["status"], "failed");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("rejected unsafe isolation flag")),
        "response should carry unsafe isolation error: {response:#}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let failed = lifecycle_event(&stderr, "app_action.failed");
    assert_lifecycle_trace_fields(&failed);
    assert_no_lifecycle_event(&stderr, "app_action.completed");
    assert_eq!(
        failed["payload"]["app_run_id"],
        "5d0a1d88-b9f9-4e8f-848e-605b86717330"
    );
}

#[test]
fn platform_smoke_adapter_reports_budget_consumption_denial_with_lifecycle() {
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("6d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("d78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("e9353847-48b3-472e-b88e-89770fcdbf7a"),
    );
    let request = AppAdapterRequest::new(
        "platform-smoke",
        "budget-consumption-smoke",
        "budget-consumption-smoke",
        Vec::new(),
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:6d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-platform-smoke"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn platform-smoke adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
        .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should serialize failed response instead of crashing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("adapter response is JSON");
    assert_eq!(response["ok"], false);
    assert_eq!(response["report"]["status"], "failed");
    assert_eq!(
        response["report"]["outputs"]["values"]["agenthero_budget_units_remaining"],
        1
    );
    assert_eq!(response["report"]["nodes"][0]["status"], "ok");
    assert_eq!(response["report"]["nodes"][1]["status"], "failed");
    assert_eq!(response["report"]["nodes"][1]["exit_status"], serde_json::Value::Null);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("budget policy requires 2 units but only 1 remain")),
        "response should carry budget denial error: {response:#}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let failed = lifecycle_event(&stderr, "app_action.failed");
    assert_lifecycle_trace_fields(&failed);
    assert_no_lifecycle_event(&stderr, "app_action.completed");
    assert_eq!(
        failed["payload"]["app_run_id"],
        "6d0a1d88-b9f9-4e8f-848e-605b86717330"
    );
}

#[test]
fn platform_smoke_adapter_emits_awaiting_approval_lifecycle_for_paused_dag() {
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("4d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("b78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("c9353847-48b3-472e-b88e-89770fcdbf7a"),
    );
    let request = AppAdapterRequest::new(
        "platform-smoke",
        "approval-pause-smoke",
        "approval-pause-smoke",
        Vec::new(),
        input,
        true,
        false,
    )
    .with_idempotency_key("app-run:4d0a1d88-b9f9-4e8f-848e-605b86717330");

    let mut child = Command::new(env!("CARGO_BIN_EXE_agenthero-dag-app-platform-smoke"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn platform-smoke adapter");
    serde_json::to_writer(child.stdin.as_mut().expect("adapter stdin"), &request)
        .expect("write adapter request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("adapter exits");
    assert!(
        output.status.success(),
        "adapter should serialize awaiting approval response: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("adapter response is JSON");
    assert_eq!(response["ok"], true);
    assert_eq!(response["report"]["status"], "awaiting_approval");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let awaiting = lifecycle_event(&stderr, "app_action.awaiting_approval");
    assert_lifecycle_trace_fields(&awaiting);
    assert_no_lifecycle_event(&stderr, "app_action.completed");
    assert_eq!(awaiting["payload"]["approved_key"], "approval/human_release");
    assert_eq!(
        awaiting["payload"]["app_run_id"],
        "4d0a1d88-b9f9-4e8f-848e-605b86717330"
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

fn assert_no_lifecycle_event(stderr: &str, event_type: &str) {
    assert!(
        stderr
            .lines()
            .filter_map(|line| line.strip_prefix(APP_ADAPTER_EVENT_PREFIX))
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .all(|event| event["event_type"] != event_type),
        "unexpected lifecycle event `{event_type}` in stderr:\n{stderr}"
    );
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
