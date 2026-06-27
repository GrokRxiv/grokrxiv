//! `CodexRunner` — stub. Not implemented in the FP-RPT3d MVP.
//!
//! TODO FP-RPT3d-followup: spawn
//! `codex exec --skip-git-repo-check --json --output-schema <path> <prompt>`
//! and tee stdout/stderr incrementally to the supervisor logs while the process
//! is still running. `--json` is required so tool activity and partial events
//! are visible instead of buffered behind a final transcript.

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
            "CodexRunner: MVP stub — not implemented. Required visible CLI shape: \
             codex exec --skip-git-repo-check --json --output-schema <schema.json> <prompt>, \
             with stdout/stderr streamed live to logs while running.\n",
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
