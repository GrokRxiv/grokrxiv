//! `ClaudeRunner` — spawn the `claude` CLI as a subprocess worker.
//!
//! Invocation (locked):
//!
//! ```text
//! claude -p - --model <model> --output-format json
//! ```
//!
//! stdin   = the rendered prompt (read from `RunnerConfig::prompt_path`)
//! stdout  = `{"type":"result","subtype":"success","result":"<text>", "total_cost_usd":..., "usage":{...}}`
//!
//! The `result` field is itself the agent's three-field JSON wrapper
//! (`review_md` + `verdict_json` + `audit_json`). The supervisor — not
//! this runner — splits that wrapper into the three output files and
//! schema-validates `verdict.json`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
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
        .arg("-")
        .arg("--model")
        .arg(&job.runner_config.model)
        .arg("--output-format")
        .arg("json")
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

    let dur = Duration::from_secs(job.timeout_seconds);
    let wait_fut = child.wait_with_output();
    let output = match timeout(dur, wait_fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            let finished_at = chrono::Utc::now();
            let _ = std::fs::write(&stderr_path, format!("supervisor: wait failed: {e}\n"));
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
        Err(_) => {
            let finished_at = chrono::Utc::now();
            let _ = std::fs::write(
                &stderr_path,
                format!(
                    "supervisor: timeout after {}s for runner=claude model={}\n",
                    job.timeout_seconds, job.runner_config.model
                ),
            );
            let _ = std::fs::write(&stdout_path, "");
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
    let stdout_bytes = output.stdout;
    let stderr_bytes = output.stderr;
    let _ = std::fs::write(&stdout_path, &stdout_bytes);
    let _ = std::fs::write(&stderr_path, &stderr_bytes);

    if !output.status.success() {
        return AgentRunResult {
            status: RunStatus::Failed,
            stdout_path,
            stderr_path,
            output_files: Vec::new(),
            started_at,
            finished_at,
            exit_code: output.status.code(),
        };
    }

    let stdout_text = String::from_utf8_lossy(&stdout_bytes).to_string();

    // Step 1: parse the claude wrapper.
    let wrapper: serde_json::Value = match serde_json::from_str(stdout_text.trim()) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::write(
                &stderr_path,
                format!(
                    "{}\n--- supervisor: failed to parse claude wrapper JSON: {e} ---\n",
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
                exit_code: output.status.code(),
            };
        }
    };
    let result_text = match wrapper.get("result").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            let _ = std::fs::write(
                &stderr_path,
                format!(
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
                exit_code: output.status.code(),
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
            exit_code: output.status.code(),
        },
        Err(super::supervisor::MaterialiseError::Invalid(msg)) => {
            let _ = std::fs::write(
                &stderr_path,
                format!(
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
                exit_code: output.status.code(),
            }
        }
        Err(super::supervisor::MaterialiseError::Failed(msg)) => {
            let _ = std::fs::write(
                &stderr_path,
                format!(
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
                exit_code: output.status.code(),
            }
        }
    }
}
