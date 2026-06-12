//! Process adapter for the GrokRxiv DAG app.

#![forbid(unsafe_code)]

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_PROTOCOL};
use agenthero_app_sdk::{
    load_dag_manifest, read_adapter_request, resolve_app_root, resolve_runtime_binary,
    write_adapter_response,
};
use agenthero_dag_executor::{
    manifest_node_result, DagExecutionReport, DagExecutor, NodeExecutionContext,
    NodeExecutionResult, NodeHandler,
};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Debug, Clone)]
struct GrokrxivAdapter {
    app_name: &'static str,
}

#[derive(Debug)]
struct RuntimeProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
    let request = read_adapter_request(std::io::stdin())?;
    let response = match run(&request).await {
        Ok(response) => response,
        Err(err) => AppAdapterResponse::failed(&request, format!("{err:#}")),
    };
    write_adapter_response(std::io::stdout(), &response)?;
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
    let manifest = load_dag_manifest(app_root(), &request.dag_type)?;
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
    let stream_stderr = runtime_stderr_stream_requested(request);
    let mut command = runtime_command(&runtime_manifest);
    build_runtime_args(&mut command, request);
    let output = match run_runtime_process(command, stream_stderr).await {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound && runtime_fallback_allowed() => {
            let mut fallback = runtime_fallback_command(&runtime_manifest);
            build_runtime_args(&mut fallback, request);
            run_runtime_process(fallback, stream_stderr).await.map_err(|fallback_err| {
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
    if runtime_debug_logs_requested(request) {
        command.arg("--debug-logs");
    }
    if runtime_status_requested(request) || review_debug_requested(request) {
        command.arg("--status");
    }
    command.arg(&request.action).args(&request.args);
}

async fn run_runtime_process(
    mut command: tokio::process::Command,
    tee_stderr: bool,
) -> std::io::Result<RuntimeProcessOutput> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(ErrorKind::Other, "runtime stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::new(ErrorKind::Other, "runtime stderr unavailable"))?;
    let stdout_task = tokio::spawn(async move { read_runtime_pipe(stdout, false).await });
    let stderr_task = tokio::spawn(async move { read_runtime_pipe(stderr, tee_stderr).await });
    let status = child.wait().await?;
    let stdout = stdout_task
        .await
        .map_err(|err| std::io::Error::new(ErrorKind::Other, format!("join stdout: {err}")))??;
    let stderr = stderr_task
        .await
        .map_err(|err| std::io::Error::new(ErrorKind::Other, format!("join stderr: {err}")))??;
    Ok(RuntimeProcessOutput {
        status,
        stdout,
        stderr,
    })
}

async fn read_runtime_pipe(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    tee_stderr: bool,
) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut stderr = tokio::io::stderr();
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if tee_stderr {
            stderr.write_all(&buf[..n]).await?;
            stderr.flush().await?;
        }
    }
    Ok(out)
}

fn runtime_status_requested(request: &AppAdapterRequest) -> bool {
    request
        .input
        .values
        .get("stream_stderr")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn runtime_debug_logs_requested(request: &AppAdapterRequest) -> bool {
    request
        .input
        .values
        .get("debug_logs")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn review_debug_requested(request: &AppAdapterRequest) -> bool {
    request.args.iter().any(|arg| arg == "--debug")
}

fn runtime_stderr_stream_requested(request: &AppAdapterRequest) -> bool {
    runtime_status_requested(request)
        || runtime_debug_logs_requested(request)
        || review_debug_requested(request)
}

fn runtime_fallback_allowed() -> bool {
    cfg!(debug_assertions)
        || std::env::var("AGENTHERO_ALLOW_ADAPTER_FALLBACK")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn app_root() -> PathBuf {
    resolve_app_root("grokrxiv", env!("CARGO_MANIFEST_DIR"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use agenthero_dag_executor::DagIo;

    fn request_with_flags(stream: bool, debug_logs: bool, args: Vec<String>) -> AppAdapterRequest {
        let mut input = DagIo::default();
        input
            .values
            .insert("stream_stderr".to_string(), serde_json::json!(stream));
        input
            .values
            .insert("debug_logs".to_string(), serde_json::json!(debug_logs));
        AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            args,
            input,
            false,
            false,
        )
    }

    fn request_with_stream(stream: bool) -> AppAdapterRequest {
        request_with_flags(
            stream,
            false,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )
    }

    #[test]
    fn runtime_status_requested_follows_adapter_input_flag() {
        assert!(runtime_status_requested(&request_with_stream(true)));
        assert!(!runtime_status_requested(&request_with_stream(false)));
    }

    #[test]
    fn runtime_debug_logs_requested_follows_adapter_input_flag() {
        assert!(runtime_debug_logs_requested(&request_with_flags(
            false,
            true,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )));
        assert!(!runtime_debug_logs_requested(&request_with_flags(
            false,
            false,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )));
    }

    #[test]
    fn runtime_stderr_streaming_includes_review_debug_flag() {
        assert!(runtime_stderr_stream_requested(&request_with_flags(
            false,
            false,
            vec![
                "2606.00799".to_string(),
                "--loop".to_string(),
                "--debug".to_string()
            ],
        )));
    }
}
