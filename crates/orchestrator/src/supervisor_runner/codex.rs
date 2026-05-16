//! `CodexRunner` — stub. Not implemented in the FP-RPT3d MVP.
//!
//! TODO FP-RPT3d-followup: spawn `codex exec --json --output-schema <path>`
//! with the same supervisor contract as ClaudeRunner.

use async_trait::async_trait;

use super::{AgentJob, AgentRunResult, AgentRunner, Capabilities, RunStatus, Stage};

/// Placeholder for the future Codex CLI runner. Every `run()` returns
/// `RunStatus::Failed` with a clear message.
pub struct CodexRunner;

impl CodexRunner {
    /// Construct the stub.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodexRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for CodexRunner {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stages: vec![Stage::Review],
            supports_timeout: true,
            supports_structured_output: true,
        }
    }

    async fn run(&self, job: AgentJob) -> AgentRunResult {
        let now = chrono::Utc::now();
        let stderr_path = job.output_dir.join("..").join("logs").join("stderr.log");
        let stdout_path = job.output_dir.join("..").join("logs").join("stdout.log");
        let _ = std::fs::write(
            &stderr_path,
            "CodexRunner: MVP stub — not implemented (see FP-RPT3d-followup).\n",
        );
        let _ = std::fs::write(&stdout_path, "");
        AgentRunResult {
            status: RunStatus::Failed,
            stdout_path,
            stderr_path,
            output_files: Vec::new(),
            started_at: now,
            finished_at: now,
            exit_code: None,
        }
    }
}
