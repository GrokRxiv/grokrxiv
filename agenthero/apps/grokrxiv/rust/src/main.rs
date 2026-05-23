//! Process adapter for the GrokRxiv DAG app.

#![forbid(unsafe_code)]

use std::io::ErrorKind;
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
    let mut command = runtime_command(&runtime_manifest);
    build_runtime_args(&mut command, request);
    let output = match command.output().await {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound && runtime_fallback_allowed() => {
            let mut fallback = runtime_fallback_command(&runtime_manifest);
            build_runtime_args(&mut fallback, request);
            fallback.output().await.map_err(|fallback_err| {
                anyhow::anyhow!(
                    "run GrokRxiv app action `{}`: compiled binary was not found and cargo fallback failed: {fallback_err}",
                    request.action
                )
            })?
        }
        Err(err) => {
            return Err(anyhow::anyhow!(
                "run GrokRxiv app action `{}` with compiled grokrxiv-app binary: {err}",
                request.action
            ));
        }
    };
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

fn runtime_command(runtime_manifest: &Path) -> tokio::process::Command {
    let mut command =
        tokio::process::Command::new(resolve_runtime_binary("GROKRXIV_APP_BIN", "grokrxiv-app"));
    if let Some(parent) = runtime_manifest.parent() {
        command.current_dir(parent);
    }
    set_app_root_env(&mut command);
    command
}

fn runtime_fallback_command(runtime_manifest: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("cargo");
    command
        .arg("run")
        .arg("--manifest-path")
        .arg(runtime_manifest)
        .arg("--quiet")
        .arg("--bin")
        .arg("grokrxiv-app")
        .arg("--")
        .current_dir(repo_root());
    set_app_root_env(&mut command);
    command
}

fn build_runtime_args(command: &mut tokio::process::Command, request: &AppAdapterRequest) {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if request.json {
        command.arg("--json");
    }
    if request.dry_run {
        command.arg("--dry-run");
    }
    command.arg(&request.action).args(&request.args);
}

fn runtime_fallback_allowed() -> bool {
    cfg!(debug_assertions)
        || std::env::var("AGENTHERO_ALLOW_ADAPTER_FALLBACK")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn resolve_runtime_binary(env_key: &str, name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_key).map(PathBuf::from) {
        return path;
    }
    if let Some(path) = std::env::var_os("AGENTHERO_APP_BIN_DIR").map(PathBuf::from) {
        let candidate = path.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(name)
}

fn app_root() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENTHERO_APP_ROOT").map(PathBuf::from) {
        return if path.is_absolute() {
            path
        } else {
            resolve_relative_path(&path)
        };
    }
    if let Some(path) = std::env::var_os("AGENTHERO_APPS_ROOT").map(PathBuf::from) {
        let apps_root = if path.is_absolute() {
            path
        } else {
            resolve_relative_path(&path)
        };
        let candidate = apps_root.join("grokrxiv");
        if candidate.join("app.yaml").is_file() {
            return candidate;
        }
    }
    discover_app_root().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    })
}

fn repo_root() -> PathBuf {
    app_root()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn set_app_root_env(command: &mut tokio::process::Command) {
    let app_root = app_root();
    command.env("AGENTHERO_APP_ROOT", &app_root);
    if let Some(apps_root) = app_root.parent() {
        command.env("AGENTHERO_APPS_ROOT", apps_root);
    }
}

fn resolve_relative_path(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_candidate = cwd.join(path);
    if cwd_candidate.exists() {
        return cwd_candidate;
    }
    if let Some(workspace) = discover_workspace_root() {
        let workspace_candidate = workspace.join(path);
        if workspace_candidate.exists() {
            return workspace_candidate;
        }
    }
    cwd_candidate
}

fn discover_app_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    starts.into_iter().find_map(|start| {
        start.ancestors().find_map(|candidate| {
            let direct = candidate.join("app.yaml");
            if direct.is_file()
                && candidate.file_name().and_then(|n| n.to_str()) == Some("grokrxiv")
            {
                return Some(candidate.to_path_buf());
            }
            let nested = candidate.join("agenthero/apps/grokrxiv");
            nested.join("app.yaml").is_file().then_some(nested)
        })
    })
}

fn discover_workspace_root() -> Option<PathBuf> {
    discover_app_root().and_then(|app| {
        app.parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    })
}
