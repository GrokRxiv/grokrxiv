//! Process adapter for the C2Rust DAG app.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use agenthero_agent_runtime::{
    app_adapter_lifecycle_event, write_adapter_event, AppAdapterRequest, AppAdapterResponse,
};
use agenthero_app_sdk::{
    app_root_from_manifest_dir, load_dag_manifest, read_adapter_request, write_adapter_response,
};
use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagExecutionReport, DagExecutor};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let request = read_adapter_request(std::io::stdin())?;
    emit_app_lifecycle_event(
        &request,
        "info",
        "app_action.started",
        format!("c2rust action `{}` started", request.action),
        "running",
        None,
        BTreeMap::new(),
    );
    let response = match run(&request).await {
        Ok(report) => {
            emit_app_lifecycle_event(
                &request,
                "info",
                "app_action.completed",
                format!("c2rust action `{}` completed", request.action),
                "completed",
                Some(0),
                BTreeMap::from([("node_count".to_string(), json!(report.nodes.len()))]),
            );
            AppAdapterResponse::ok_report(&request, report)
        }
        Err(err) => {
            let message = format!("{err:#}");
            emit_app_lifecycle_event(
                &request,
                "error",
                "app_action.failed",
                format!("c2rust action `{}` failed: {message}", request.action),
                "failed",
                Some(1),
                BTreeMap::from([("error".to_string(), json!(message))]),
            );
            AppAdapterResponse::failed(&request, message)
        }
    };
    write_adapter_response(std::io::stdout(), &response)?;
    Ok(())
}

fn emit_app_lifecycle_event(
    request: &AppAdapterRequest,
    level: &str,
    event_type: &str,
    message: String,
    status: &str,
    exit_status: Option<i32>,
    extra: BTreeMap<String, serde_json::Value>,
) {
    let event = app_adapter_lifecycle_event(
        request,
        level,
        event_type,
        message,
        status,
        exit_status,
        extra,
    );
    let _ = write_adapter_event(std::io::stderr(), &event);
}

async fn run(request: &AppAdapterRequest) -> anyhow::Result<DagExecutionReport> {
    if request.app != "c2rust" {
        anyhow::bail!("c2rust adapter received app `{}`", request.app);
    }
    if request.dag_type != "c2rust" {
        anyhow::bail!("c2rust adapter received dag_type `{}`", request.dag_type);
    }
    let manifest = load_dag_manifest(app_root(), "c2rust")?;
    let mut input = request.input.clone();
    if let Some(source) = request.args.first() {
        input.values.insert("source".to_string(), json!(source));
    }
    input.values.insert(
        "adapter_idempotency_key".to_string(),
        json!(request.idempotency_key),
    );
    DagExecutor::new(C2RustDagApp::default())
        .with_event_sink(|event| {
            let _ = write_adapter_event(std::io::stderr(), &event);
        })
        .execute_with_checkpoint(&manifest, input, request.checkpoint.as_ref())
        .await
}

fn app_root() -> std::path::PathBuf {
    app_root_from_manifest_dir(env!("CARGO_MANIFEST_DIR"))
}
