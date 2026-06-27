//! `ClaudeRunner` — spawn the `claude` CLI as a subprocess worker.
//!
//! Invocation (locked):
//!
//! ```text
//! claude -p <query> --model <model> --effort <level> --output-format stream-json --verbose
//!   --include-partial-messages --include-hook-events
//!   --safe-mode --disable-slash-commands --mcp-config '{"mcpServers":{}}'
//!   --strict-mcp-config --tools ''
//! ```
//!
//! stdin   = the rendered prompt (read from `RunnerConfig::prompt_path`)
//! stdout  = Claude stream JSONL events, including tool use, partial messages,
//! and a final result event.
//!
//! The `result` field is itself the agent's three-field JSON wrapper
//! (`review_md` + `verdict_json` + `audit_json`). The supervisor — not
//! this runner — splits that wrapper into the three output files and
//! schema-validates `verdict.json`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use super::{AgentJob, AgentRunResult, AgentRunner, Capabilities, RunStatus, Stage};

/// Subprocess runner for the `claude` CLI (Max subscription path).
pub struct ClaudeRunner {
    binary: String,
}

impl ClaudeRunner {
    /// Construct a runner that spawns the `claude` binary on PATH.
    pub fn new() -> Self {
        Self {
            binary: std::env::var("AGENTHERO_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
        }
    }

    /// Construct a runner that spawns a specific binary (used in tests).
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl Default for ClaudeRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for ClaudeRunner {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stages: vec![Stage::Review],
            supports_timeout: true,
            supports_structured_output: true,
        }
    }

    async fn run(&self, job: AgentJob) -> AgentRunResult {
        run_inner(&self.binary, job).await
    }
}

async fn run_inner(binary: &str, job: AgentJob) -> AgentRunResult {
    let started_at = chrono::Utc::now();
    let stdout_path = job.output_dir.join("..").join("logs").join("stdout.log");
    let stderr_path = job.output_dir.join("..").join("logs").join("stderr.log");
    if let Some(parent) = stdout_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Some(parent) = stderr_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let prompt = match std::fs::read_to_string(&job.runner_config.prompt_path) {
        Ok(s) => s,
        Err(e) => {
            let finished_at = chrono::Utc::now();
            let _ = std::fs::write(
                &stderr_path,
                format!(
                    "supervisor: could not read prompt {}: {e}\n",
                    job.runner_config.prompt_path.display()
                ),
            );
            let _ = std::fs::write(&stdout_path, "");
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
    };

    let mut cmd = Command::new(binary);
    cmd.arg("-p")
        .arg(
            "Read the complete AgentHero prompt supplied on stdin. Use visible tool actions when \
             tools are available and return the required JSON artifact.",
        )
        .arg("--model")
        .arg(&job.runner_config.model)
        .arg("--effort")
        .arg(claude_effort())
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--include-partial-messages")
        .arg("--include-hook-events")
        .arg("--safe-mode")
        .arg("--disable-slash-commands")
        .arg("--mcp-config")
        .arg("{\"mcpServers\":{}}")
        .arg("--strict-mcp-config")
        .arg("--tools")
        .arg("")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let finished_at = chrono::Utc::now();
            let _ = std::fs::write(
                &stderr_path,
                format!("supervisor: failed to spawn `{binary}`: {e}\n"),
            );
            let _ = std::fs::write(&stdout_path, "");
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes()).await;
        drop(stdin);
    }

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let finished_at = chrono::Utc::now();
            let _ = append_log(
                &stderr_path,
                "supervisor: failed to capture claude stdout\n",
            );
            let _ = std::fs::write(&stdout_path, "");
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let finished_at = chrono::Utc::now();
            let _ = append_log(
                &stderr_path,
                "supervisor: failed to capture claude stderr\n",
            );
            let _ = std::fs::write(&stdout_path, "");
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
    };
    let stdout_for_task = stdout_path.clone();
    let stderr_for_task = stderr_path.clone();
    let stdout_task =
        tokio::spawn(async move { read_stream_to_live_log(stdout, stdout_for_task).await });
    let stderr_task =
        tokio::spawn(async move { read_stream_to_live_log(stderr, stderr_for_task).await });

    let dur = Duration::from_secs(job.timeout_seconds);
    let status = match timeout(dur, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            let finished_at = chrono::Utc::now();
            let _ = append_log(&stderr_path, &format!("supervisor: wait failed: {e}\n"));
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
        Err(_) => {
            let _ = child.kill().await;
            let finished_at = chrono::Utc::now();
            let _ = append_log(
                &stderr_path,
                &format!(
                    "supervisor: timeout after {}s for runner=claude model={}\n",
                    job.timeout_seconds, job.runner_config.model
                ),
            );
            stdout_task.abort();
            stderr_task.abort();
            return AgentRunResult {
                status: RunStatus::Timeout,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: None,
            };
        }
    };

    let finished_at = chrono::Utc::now();
    let stdout_bytes = match stdout_task.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            let _ = append_log(
                &stderr_path,
                &format!("supervisor: stdout read failed: {e}\n"),
            );
            Vec::new()
        }
        Err(e) => {
            let _ = append_log(
                &stderr_path,
                &format!("supervisor: stdout task failed: {e}\n"),
            );
            Vec::new()
        }
    };
    let stderr_bytes = match stderr_task.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            let _ = append_log(
                &stderr_path,
                &format!("supervisor: stderr read failed: {e}\n"),
            );
            Vec::new()
        }
        Err(e) => {
            let _ = append_log(
                &stderr_path,
                &format!("supervisor: stderr task failed: {e}\n"),
            );
            Vec::new()
        }
    };

    if !status.success() {
        return AgentRunResult {
            status: RunStatus::Failed,
            stdout_path,
            stderr_path,
            output_files: Vec::new(),
            started_at,
            finished_at,
            exit_code: status.code(),
        };
    }

    let stdout_text = String::from_utf8_lossy(&stdout_bytes).to_string();

    // Step 1: parse the final Claude stream result.
    let result_text = match claude_result_text(&stdout_text) {
        Some(s) => s.to_string(),
        None => {
            let _ = append_log(
                &stderr_path,
                &format!(
                    "{}\n--- supervisor: claude wrapper missing string `.result` ---\n",
                    String::from_utf8_lossy(&stderr_bytes)
                ),
            );
            return AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: status.code(),
            };
        }
    };

    // Step 2: hand the inner agent JSON to the shared materialiser.
    match super::supervisor::materialise_agent_output(&job, &result_text) {
        Ok(files) => AgentRunResult {
            status: RunStatus::Success,
            stdout_path,
            stderr_path,
            output_files: files,
            started_at,
            finished_at,
            exit_code: status.code(),
        },
        Err(super::supervisor::MaterialiseError::Invalid(msg)) => {
            let _ = append_log(
                &stderr_path,
                &format!(
                    "{}\n--- supervisor: invalid agent output ---\n{msg}\n",
                    String::from_utf8_lossy(&stderr_bytes)
                ),
            );
            AgentRunResult {
                status: RunStatus::InvalidOutput,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: status.code(),
            }
        }
        Err(super::supervisor::MaterialiseError::Failed(msg)) => {
            let _ = append_log(
                &stderr_path,
                &format!(
                    "{}\n--- supervisor: materialisation failed ---\n{msg}\n",
                    String::from_utf8_lossy(&stderr_bytes)
                ),
            );
            AgentRunResult {
                status: RunStatus::Failed,
                stdout_path,
                stderr_path,
                output_files: Vec::new(),
                started_at,
                finished_at,
                exit_code: status.code(),
            }
        }
    }
}

async fn read_stream_to_live_log<R>(
    mut reader: R,
    path: std::path::PathBuf,
) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        file.write_all(&chunk[..n]).await?;
        file.flush().await?;
    }
    Ok(out)
}

fn append_log(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, text.as_bytes())
}

fn claude_result_text(stdout_text: &str) -> Option<String> {
    let trimmed = stdout_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return claude_result_from_value(&value);
    }
    let mut last = None;
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(result) = claude_result_from_value(&value) {
            last = Some(result);
        }
    }
    last
}

fn claude_result_from_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("structured_output")
        .map(|structured| structured.to_string())
        .or_else(|| {
            value
                .get("result")
                .and_then(|result| result.as_str())
                .map(str::to_string)
        })
}

fn claude_effort() -> String {
    for env_var in ["AGENTHERO_CLAUDE_EFFORT", "GROKRXIV_CLAUDE_EFFORT"] {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "low" | "medium" | "high" | "xhigh" | "max") {
                return value;
            }
        }
    }
    "medium".to_string()
}
