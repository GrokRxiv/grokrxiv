//! `GeminiRunner` — stub. Not implemented in the FP-RPT3d MVP.
//!
//! TODO FP-RPT3d-followup: if this supervisor-runner path survives, route the
//! Gemini-family Antigravity (`agy`) transport through the same supervisor
//! contract as ClaudeRunner.

use async_trait::async_trait;

use super::{AgentJob, AgentRunResult, AgentRunner, Capabilities, RunStatus, Stage};

/// Placeholder for the future Gemini-family Antigravity runner.
pub struct GeminiRunner;

impl GeminiRunner {
    /// Construct the stub.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GeminiRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for GeminiRunner {
    fn name(&self) -> &'static str {
        "gemini"
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
            "GeminiRunner: MVP stub — not implemented (see FP-RPT3d-followup).\n",
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
