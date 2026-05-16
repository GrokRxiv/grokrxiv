//! `LocalCommandRunner` — spawn a whitelisted local binary (or shell
//! script) as the agent worker.
//!
//! The runner treats `RunnerConfig::model` as the path/name of the binary
//! to spawn. The supervisor passes:
//!   - `argv[1] = input_dir`
//!   - `argv[2] = output_dir`
//!   - `argv[3] = prompt_path`
//!   - stdin    = the rendered prompt
//!
//! The child is expected to print a single JSON object to stdout with the
//! three fields the agent contract specifies (`review_md`, `verdict_json`,
//! `audit_json`). The supervisor — not this runner — splits that JSON into
//! the three output files. This keeps the runner thin and the contract
//! identical across `LocalCommandRunner`, `ClaudeRunner`, and the deferred
//! Codex/Gemini runners.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::{AgentJob, AgentRunResult, AgentRunner, Capabilities, RunStatus, Stage};

/// Whitelisted-binary runner used for tests and operator-local agents.
pub struct LocalCommandRunner;

impl LocalCommandRunner {
    /// Construct a fresh runner. Stateless — no fields.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for LocalCommandRunner {
    fn name(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stages: vec![Stage::Review, Stage::Extract, Stage::Validate],
            supports_timeout: true,
            supports_structured_output: true,
        }
    }

    async fn run(&self, job: AgentJob) -> AgentRunResult {
        run_inner(job).await
    }
}

async fn run_inner(job: AgentJob) -> AgentRunResult {
    let started_at = chrono::Utc::now();
    let stdout_path = job.output_dir.join("..").join("logs").join("stdout.log");
    let stderr_path = job.output_dir.join("..").join("logs").join("stderr.log");

    let binary = job.runner_config.model.clone();
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

    let mut cmd = Command::new(&binary);
    cmd.arg(&job.input_dir)
        .arg(&job.output_dir)
        .arg(&job.runner_config.prompt_path)
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
            // Timed out — wait_with_output already took ownership of child;
            // because we set `kill_on_drop(true)` above, returning here is
            // sufficient to SIGTERM the child as the future is dropped.
            let finished_at = chrono::Utc::now();
            let _ = std::fs::write(
                &stderr_path,
                format!(
                    "supervisor: timeout after {}s for runner=local binary={binary}\n",
                    job.timeout_seconds
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

    // Hand stdout back to the supervisor. The supervisor parses the
    // agent's three-field JSON wrapper, fans it out into the three files,
    // and validates the schema. We just need to surface success here.
    let stdout_text = String::from_utf8_lossy(&stdout_bytes).to_string();
    match super::supervisor::materialise_agent_output(&job, &stdout_text) {
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
