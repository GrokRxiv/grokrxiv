//! Process adapter for the AgentHero platform smoke DAG app.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{
    app_adapter_lifecycle_event, write_adapter_event, AppAdapterRequest, AppAdapterResponse,
    APP_ADAPTER_PROTOCOL,
};
use agenthero_app_sdk::{
    app_root_from_manifest_dir, load_dag_manifest, read_adapter_request, write_adapter_response,
};
use agenthero_dag_executor::{DagExecutionReport, DagExecutor, DagIo, GenericToolRunner};
use agenthero_dag_runtime::{DagManifest, DagNodeStatus};
use serde_json::json;

const APP_ID: &str = "platform-smoke";
const TOOL_POLICY_SMOKE_DAG: &str = "tool-policy-smoke";
const BUDGET_CONSUMPTION_SMOKE_DAG: &str = "budget-consumption-smoke";
const VERIFICATION_ROUTING_SMOKE_DAG: &str = "verification-routing-smoke";
const CANCELLATION_SMOKE_DAG: &str = "cancellation-smoke";
const APPROVAL_PAUSE_SMOKE_DAG: &str = "approval-pause-smoke";
const POLICY_DENIAL_SMOKE_DAG: &str = "policy-denial-smoke";
const ISOLATION_BOUNDARY_SMOKE_DAG: &str = "isolation-boundary-smoke";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let request = read_adapter_request(std::io::stdin())?;
    emit_app_lifecycle_event(
        &request,
        "info",
        "app_action.started",
        format!("platform-smoke action `{}` started", request.action),
        "running",
        None,
        BTreeMap::new(),
    );
    let response = match run(&request).await {
        Ok(report) => {
            if report.status == DagNodeStatus::AwaitingApproval {
                let approved_key = report_approval_key(&report);
                let mut extra = BTreeMap::from([
                    ("node_count".to_string(), json!(report.nodes.len())),
                    ("approved_key".to_string(), json!(approved_key)),
                ]);
                if let Some(node_id) = report_approval_node_id(&report) {
                    extra.insert("approval_node_id".to_string(), json!(node_id));
                }
                emit_app_lifecycle_event(
                    &request,
                    "info",
                    "app_action.awaiting_approval",
                    format!(
                        "platform-smoke action `{}` awaiting approval",
                        request.action
                    ),
                    "awaiting_approval",
                    None,
                    extra,
                );
                AppAdapterResponse::ok_report(&request, report)
            } else if let Some(message) = report_failure_message(&report) {
                emit_app_lifecycle_event(
                    &request,
                    "error",
                    "app_action.failed",
                    format!("platform-smoke action `{}` failed: {message}", request.action),
                    "failed",
                    Some(1),
                    BTreeMap::from([
                        ("error".to_string(), json!(message.clone())),
                        ("node_count".to_string(), json!(report.nodes.len())),
                    ]),
                );
                failed_report_response(&request, report, message)
            } else {
                emit_app_lifecycle_event(
                    &request,
                    "info",
                    "app_action.completed",
                    format!("platform-smoke action `{}` completed", request.action),
                    "completed",
                    Some(0),
                    BTreeMap::from([("node_count".to_string(), json!(report.nodes.len()))]),
                );
                AppAdapterResponse::ok_report(&request, report)
            }
        }
        Err(err) => {
            let message = format!("{err:#}");
            emit_app_lifecycle_event(
                &request,
                "error",
                "app_action.failed",
                format!("platform-smoke action `{}` failed: {message}", request.action),
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

fn failed_report_response(
    request: &AppAdapterRequest,
    report: DagExecutionReport,
    message: String,
) -> AppAdapterResponse {
    AppAdapterResponse {
        protocol: APP_ADAPTER_PROTOCOL.to_string(),
        app: request.app.clone(),
        action: request.action.clone(),
        dag_type: request.dag_type.clone(),
        ok: false,
        report: Some(report),
        output: None,
        error: Some(message),
    }
}

fn report_failure_message(report: &DagExecutionReport) -> Option<String> {
    if report.status != DagNodeStatus::Failed {
        return None;
    }
    if let Some(node) = report
        .nodes
        .iter()
        .find(|node| node.status == DagNodeStatus::Failed)
    {
        if let Some(error) = node.error.as_deref().filter(|error| !error.is_empty()) {
            return Some(error.to_string());
        }
        return Some(format!(
            "DAG `{}` failed at node `{}`",
            report.dag_type, node.node_id
        ));
    }
    if let Some(event) = report
        .events
        .iter()
        .find(|event| event.level == "error")
        .and_then(|event| event.message.as_deref())
        .filter(|message| !message.is_empty())
    {
        return Some(event.to_string());
    }
    Some(format!("DAG `{}` failed", report.dag_type))
}

fn report_approval_key(report: &DagExecutionReport) -> String {
    report
        .nodes
        .iter()
        .find(|node| node.status == DagNodeStatus::AwaitingApproval)
        .and_then(|node| node.tool.as_deref())
        .map(|tool| format!("approval/{tool}"))
        .unwrap_or_else(|| "approval/unknown".to_string())
}

fn report_approval_node_id(report: &DagExecutionReport) -> Option<String> {
    report
        .nodes
        .iter()
        .find(|node| node.status == DagNodeStatus::AwaitingApproval)
        .map(|node| node.node_id.clone())
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
    if request.app != APP_ID {
        anyhow::bail!("{APP_ID} adapter received app `{}`", request.app);
    }
    if !matches!(
        request.dag_type.as_str(),
        TOOL_POLICY_SMOKE_DAG
            | BUDGET_CONSUMPTION_SMOKE_DAG
            | VERIFICATION_ROUTING_SMOKE_DAG
            | CANCELLATION_SMOKE_DAG
            | APPROVAL_PAUSE_SMOKE_DAG
            | POLICY_DENIAL_SMOKE_DAG
            | ISOLATION_BOUNDARY_SMOKE_DAG
    ) {
        anyhow::bail!(
            "{APP_ID} adapter received dag_type `{}`",
            request.dag_type
        );
    }

    let mut manifest = load_dag_manifest(app_root(), &request.dag_type)?;
    let mut input = request.input.clone();
    seed_smoke_input(&mut input, request);
    if request.dag_type == VERIFICATION_ROUTING_SMOKE_DAG {
        let toolchain_bin = install_fake_verification_toolchain()?;
        pin_verification_tool_commands(&mut manifest, &toolchain_bin)?;
    } else if request.dag_type == ISOLATION_BOUNDARY_SMOKE_DAG {
        let toolchain_bin = install_fake_isolation_toolchain()?;
        pin_isolation_tool_commands(&mut manifest, &toolchain_bin)?;
    }

    DagExecutor::new(GenericToolRunner::new(generic_tool_artifact_root()))
        .with_event_sink(|event| {
            let _ = write_adapter_event(std::io::stderr(), &event);
        })
        .execute_with_checkpoint(&manifest, input, request.checkpoint.as_ref())
        .await
}

fn install_fake_verification_toolchain() -> anyhow::Result<PathBuf> {
    let bin_root = generic_tool_artifact_root().join("toolchain").join("bin");
    std::fs::create_dir_all(&bin_root)?;
    let bin = bin_root.canonicalize()?;
    write_executable(
        &bin.join("lean"),
        "#!/bin/sh\nprintf '{\"tool\":\"%s\",\"executor\":\"%s\",\"dag\":\"%s\",\"node\":\"%s\"}\\n' \"$AGENTHERO_TOOL_ID\" \"$AGENTHERO_EXECUTOR_KIND\" \"$AGENTHERO_DAG_TYPE\" \"$AGENTHERO_NODE_ID\" > lean_verification.json\n",
    )?;
    write_executable(
        &bin.join("runhaskell"),
        "#!/bin/sh\nprintf '{\"tool\":\"%s\",\"executor\":\"%s\",\"dag\":\"%s\",\"node\":\"%s\"}\\n' \"$AGENTHERO_TOOL_ID\" \"$AGENTHERO_EXECUTOR_KIND\" \"$AGENTHERO_DAG_TYPE\" \"$AGENTHERO_NODE_ID\" > haskell_verification.json\n",
    )?;
    let previous_path = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &previous_path,
    )))?;
    std::env::set_var("PATH", joined);
    Ok(bin)
}

fn pin_verification_tool_commands(manifest: &mut DagManifest, bin: &Path) -> anyhow::Result<()> {
    for tool in &mut manifest.tools {
        match tool.id.as_str() {
            "lean_kernel" => {
                tool.command = Some(vec![
                    bin.join("lean").to_string_lossy().into_owned(),
                    "Proof.lean".to_string(),
                ]);
            }
            "haskell_checker" => {
                tool.command = Some(vec![
                    bin.join("runhaskell").to_string_lossy().into_owned(),
                    "Check.hs".to_string(),
                ]);
            }
            _ => {}
        }
    }

    for tool_id in ["lean_kernel", "haskell_checker"] {
        if !manifest.tools.iter().any(|tool| tool.id == tool_id) {
            anyhow::bail!("verification routing smoke missing tool `{tool_id}`");
        }
    }

    Ok(())
}

fn install_fake_isolation_toolchain() -> anyhow::Result<PathBuf> {
    let bin_root = generic_tool_artifact_root()
        .join("isolation-toolchain")
        .join("bin");
    std::fs::create_dir_all(&bin_root)?;
    let bin = bin_root.canonicalize()?;
    write_executable(
        &bin.join("docker"),
        "#!/bin/sh\nprintf '{\"spawned\":\"docker\"}\\n' > docker_should_not_spawn.json\n",
    )?;
    write_executable(
        &bin.join("wasmtime"),
        "#!/bin/sh\nprintf '{\"spawned\":\"wasm\"}\\n' > wasm_should_not_spawn.json\n",
    )?;
    Ok(bin)
}

fn pin_isolation_tool_commands(manifest: &mut DagManifest, bin: &Path) -> anyhow::Result<()> {
    for tool in &mut manifest.tools {
        match tool.id.as_str() {
            "unsafe_docker" => {
                tool.command = Some(vec![
                    bin.join("docker").to_string_lossy().into_owned(),
                    "run".to_string(),
                    "--privileged".to_string(),
                    "agenthero/test".to_string(),
                ]);
            }
            "unsafe_wasm" => {
                tool.command = Some(vec![
                    bin.join("wasmtime").to_string_lossy().into_owned(),
                    "--dir=/".to_string(),
                    "module.wasm".to_string(),
                ]);
            }
            _ => {}
        }
    }

    for tool_id in ["unsafe_docker", "unsafe_wasm"] {
        if !manifest.tools.iter().any(|tool| tool.id == tool_id) {
            anyhow::bail!("isolation boundary smoke missing tool `{tool_id}`");
        }
    }

    Ok(())
}

fn write_executable(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn seed_smoke_input(input: &mut DagIo, request: &AppAdapterRequest) {
    input.values.insert(
        "adapter_idempotency_key".to_string(),
        json!(request.idempotency_key),
    );
    if request.dag_type == TOOL_POLICY_SMOKE_DAG {
        input
            .values
            .insert("approval/write_policy_report".to_string(), json!(true));
        input
            .values
            .insert("approval/human_checkpoint".to_string(), json!(true));
    } else if request.dag_type == BUDGET_CONSUMPTION_SMOKE_DAG {
        input
            .values
            .entry("agenthero_budget_units_remaining".to_string())
            .or_insert_with(|| json!(3));
    }
}

fn app_root() -> PathBuf {
    app_root_from_manifest_dir(env!("CARGO_MANIFEST_DIR"))
}

fn generic_tool_artifact_root() -> PathBuf {
    std::env::var_os("AGENTHERO_RUNTIME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".agenthero"))
        .join("platform-smoke")
        .join("generic-tools")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_verification_tool_commands_uses_fixture_verifiers() {
        let mut manifest =
            load_dag_manifest(app_root(), VERIFICATION_ROUTING_SMOKE_DAG).expect("manifest loads");
        let bin = PathBuf::from("/tmp/agenthero-verifier-fixtures");

        pin_verification_tool_commands(&mut manifest, &bin).expect("commands are pinned");

        let lean = manifest
            .tools
            .iter()
            .find(|tool| tool.id == "lean_kernel")
            .and_then(|tool| tool.command.as_ref())
            .expect("lean command exists");
        assert_eq!(lean[0], "/tmp/agenthero-verifier-fixtures/lean");
        assert_eq!(lean[1], "Proof.lean");

        let haskell = manifest
            .tools
            .iter()
            .find(|tool| tool.id == "haskell_checker")
            .and_then(|tool| tool.command.as_ref())
            .expect("haskell command exists");
        assert_eq!(haskell[0], "/tmp/agenthero-verifier-fixtures/runhaskell");
        assert_eq!(haskell[1], "Check.hs");
    }

    #[test]
    fn cancellation_smoke_manifest_loads_for_cleanup_acceptance() {
        let manifest =
            load_dag_manifest(app_root(), CANCELLATION_SMOKE_DAG).expect("manifest loads");

        assert_eq!(manifest.id.as_str(), CANCELLATION_SMOKE_DAG);
        assert!(manifest
            .tools
            .iter()
            .any(|tool| tool.id == "delayed_marker"));
    }

    #[test]
    fn budget_consumption_smoke_manifest_loads_for_policy_acceptance() {
        let manifest =
            load_dag_manifest(app_root(), BUDGET_CONSUMPTION_SMOKE_DAG).expect("manifest loads");

        assert_eq!(manifest.id.as_str(), BUDGET_CONSUMPTION_SMOKE_DAG);
        assert!(manifest
            .tools
            .iter()
            .any(|tool| tool.id == "first_budgeted_tool"));
        assert!(manifest
            .tools
            .iter()
            .any(|tool| tool.id == "second_budgeted_tool"));
    }

    #[test]
    fn approval_pause_smoke_manifest_loads_for_pause_resume_acceptance() {
        let manifest =
            load_dag_manifest(app_root(), APPROVAL_PAUSE_SMOKE_DAG).expect("manifest loads");

        assert_eq!(manifest.id.as_str(), APPROVAL_PAUSE_SMOKE_DAG);
        assert!(manifest
            .tools
            .iter()
            .any(|tool| tool.id == "human_release"));
        assert!(manifest.nodes.iter().any(|node| node.id == "wait_for_release"));
    }

    #[test]
    fn policy_denial_smoke_manifest_loads_for_policy_acceptance() {
        let manifest =
            load_dag_manifest(app_root(), POLICY_DENIAL_SMOKE_DAG).expect("manifest loads");

        assert_eq!(manifest.id.as_str(), POLICY_DENIAL_SMOKE_DAG);
        assert!(manifest
            .tools
            .iter()
            .any(|tool| tool.id == "network_denied_shell"));
    }

    #[test]
    fn isolation_boundary_smoke_manifest_loads_for_tool_isolation_acceptance() {
        let manifest =
            load_dag_manifest(app_root(), ISOLATION_BOUNDARY_SMOKE_DAG).expect("manifest loads");

        assert_eq!(manifest.id.as_str(), ISOLATION_BOUNDARY_SMOKE_DAG);
        assert!(manifest.tools.iter().any(|tool| tool.id == "unsafe_docker"));
        assert!(manifest.tools.iter().any(|tool| tool.id == "unsafe_wasm"));
    }
}
