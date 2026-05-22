//! Process adapter for the GrokRxiv DAG app.

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_PROTOCOL};
use agenthero_dag_executor::{
    manifest_node_result, DagExecutionReport, DagExecutor, NodeExecutionContext,
    NodeExecutionResult, NodeHandler,
};
use agenthero_dag_runtime::DagManifest;
use async_trait::async_trait;

#[derive(Debug, Clone)]
struct GrokrxivAdapter {
    app_name: &'static str,
}

#[async_trait]
impl NodeHandler for GrokrxivAdapter {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(manifest_node_result(
            self.app_name,
            ctx.manifest.id.as_str(),
            ctx.node,
        ))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    let request: AppAdapterRequest = serde_json::from_str(&payload)?;
    let response = match run(&request).await {
        Ok(response) => response,
        Err(err) => AppAdapterResponse::failed(&request, format!("{err:#}")),
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

async fn run(request: &AppAdapterRequest) -> anyhow::Result<AppAdapterResponse> {
    if request.app != "grokrxiv" {
        anyhow::bail!("grokrxiv adapter received app `{}`", request.app);
    }
    if request.action == "validate-citations" {
        return Ok(AppAdapterResponse::ok_report(
            request,
            run_manifest_dag(request).await?,
        ));
    }
    run_app_runtime_action(request).await
}

async fn run_manifest_dag(request: &AppAdapterRequest) -> anyhow::Result<DagExecutionReport> {
    let manifest_path = app_root()
        .join("dags")
        .join(format!("{}.yaml", request.dag_type));
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|err| anyhow::anyhow!("load {}: {err}", manifest_path.display()))?;
    if manifest.id.as_str() != request.dag_type {
        anyhow::bail!(
            "manifest {} id `{}` does not match request dag_type `{}`",
            manifest_path.display(),
            manifest.id,
            request.dag_type
        );
    }
    DagExecutor::new(GrokrxivAdapter {
        app_name: "grokrxiv",
    })
    .execute(&manifest, request.input.clone())
    .await
}

async fn run_app_runtime_action(request: &AppAdapterRequest) -> anyhow::Result<AppAdapterResponse> {
    let runtime_manifest = app_root()
        .join("crates")
        .join("orchestrator")
        .join("Cargo.toml");
    let mut command = tokio::process::Command::new("cargo");
    command
        .arg("run")
        .arg("--manifest-path")
        .arg(&runtime_manifest)
        .arg("--quiet")
        .arg("--bin")
        .arg("grokrxiv-app")
        .arg("--")
        .current_dir(repo_root())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if request.json {
        command.arg("--json");
    }
    if request
        .input
        .values
        .get("dry_run")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        command.arg("--dry-run");
    }
    command.arg(&request.action).args(&request.args);
    let output = command.output().await.map_err(|err| {
        anyhow::anyhow!(
            "run GrokRxiv app action `{}` through {}: {err}",
            request.action,
            runtime_manifest.display()
        )
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        anyhow::bail!(
            "GrokRxiv action `{}` exited with {}: {}",
            request.action,
            output.status,
            stderr.trim()
        );
    }
    Ok(AppAdapterResponse {
        protocol: APP_ADAPTER_PROTOCOL.to_string(),
        app: request.app.clone(),
        action: request.action.clone(),
        dag_type: request.dag_type.clone(),
        ok: true,
        report: None,
        output: Some(serde_json::json!({
            "status": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
        })),
        error: None,
    })
}

fn app_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn repo_root() -> PathBuf {
    app_root()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
