//! FP-RPT3d MVP — operator-locked supervisor-as-parent-process runtime.
//!
//! `agh` is the parent process. Agent CLIs (claude / codex / gemini /
//! local) are child workers. Every agent call goes through the typed
//! [`AgentRunner`] interface in this module: the supervisor prepares a
//! read-only `input/` directory plus a writable `output/` directory, spawns
//! the runner exactly once, validates the output filename allowlist + the
//! `verdict.json` JSON schema, then returns an [`AgentRunResult`].
//!
//! Hard rules (locked, do not relax):
//! - Agents are leaves. They never spawn other agents.
//! - Agents may not choose paths; the supervisor picks them.
//! - Only `agh` validates and persists outputs.
//!
//! This module is intentionally separate from the manifest-driven
//! `crates/orchestrator/src/agents/{traits,runners}` code. It remains an
//! older supervisor-runner boundary while AgentHero DAG apps migrate fully to
//! the generic executor path.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod local;
pub mod prompt;
pub mod supervisor;

pub use claude::ClaudeRunner;
pub use codex::CodexRunner;
pub use gemini::GeminiRunner;
pub use local::LocalCommandRunner;
pub use supervisor::Supervisor;

/// Stage the agent is being invoked for. Only `Review` is implemented in
/// the MVP; the other two are reserved for the broader FP-RPT3d cutover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Deterministic / agent-fallback extraction.
    Extract,
    /// Review stage (the MVP focus).
    Review,
    /// Post-hoc validation pass.
    Validate,
}

/// Capability advertisement returned by every runner so the supervisor can
/// route work to the right backend.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Stages this runner can service.
    pub stages: Vec<Stage>,
    /// Whether the runner enforces a wall-clock timeout itself (vs the
    /// supervisor enforcing it externally).
    pub supports_timeout: bool,
    /// Whether the runner can be asked to emit structured output (vs free
    /// text only).
    pub supports_structured_output: bool,
}

/// One job dispatched to one runner. Construction is the supervisor's
/// responsibility; runners only consume.
#[derive(Debug, Clone)]
pub struct AgentJob {
    /// Paper this run is associated with. Used for logging / audit only —
    /// the runner does not look the paper up.
    pub paper_id: uuid::Uuid,
    /// Stage being run.
    pub stage: Stage,
    /// Read-only directory the agent reads context from. Contains at least
    /// `review_input.json` and `prompt.md`.
    pub input_dir: PathBuf,
    /// Writable directory — the agent's *only* allowed write target.
    pub output_dir: PathBuf,
    /// Per-run wall-clock timeout enforced by the supervisor via
    /// `tokio::time::timeout`. The supervisor SIGTERMs the child on expiry.
    pub timeout_seconds: u64,
    /// Runner-specific config (model, prompt path, cost ceiling).
    pub runner_config: RunnerConfig,
    /// Filename allowlist. The supervisor will flip the run to
    /// `RunStatus::InvalidOutput` if any file outside this list shows up in
    /// `output_dir`.
    pub allowed_outputs: Vec<String>,
}

/// Knobs supplied to the runner. Kept deliberately small; new fields go in
/// only when at least one runner needs them.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Model identifier for remote runners; for `LocalCommandRunner` the
    /// string is interpreted as the binary path / name to spawn.
    pub model: String,
    /// Soft cap — runners SHOULD respect this; the supervisor surfaces it
    /// in the manifest for cost accounting but does not enforce hard kills
    /// on cost overrun in the MVP.
    pub max_cost_usd: f32,
    /// Absolute path to the rendered prompt (always lives inside
    /// `input_dir`, but kept here so runners do not have to recompute it).
    pub prompt_path: PathBuf,
}

/// Terminal result returned to the supervisor.
#[derive(Debug)]
pub struct AgentRunResult {
    /// One of the four terminal states.
    pub status: RunStatus,
    /// Captured stdout (always written, even on failure).
    pub stdout_path: PathBuf,
    /// Captured stderr (always written, even on failure).
    pub stderr_path: PathBuf,
    /// Filenames the supervisor accepted as outputs. Subset of
    /// `AgentJob::allowed_outputs`.
    pub output_files: Vec<PathBuf>,
    /// Wall-clock start of the runner invocation.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Wall-clock end (either successful finish, timeout, or child crash).
    pub finished_at: chrono::DateTime<chrono::Utc>,
    /// Exit code if the child reaped naturally; `None` on timeout.
    pub exit_code: Option<i32>,
}

/// Terminal status for one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// All three output files present, verdict schema passed.
    Success,
    /// Child reported non-zero exit OR the supervisor could not parse the
    /// wrapper JSON the runner expected to see on stdout.
    Failed,
    /// Wall-clock timeout expired. Child was SIGTERM'd.
    Timeout,
    /// Output contract violated — extra files, missing files, or schema
    /// rejection on `verdict.json`.
    InvalidOutput,
}

/// The contract every runner implements. Single invocation, single
/// response — explicitly NOT a tool-call loop.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Human-readable runner name. Used for logging / manifest.
    fn name(&self) -> &'static str;
    /// What the runner can handle.
    fn capabilities(&self) -> Capabilities;
    /// Execute one job. Implementations MUST honour
    /// `AgentJob::timeout_seconds` and MUST NOT write outside
    /// `AgentJob::output_dir`.
    async fn run(&self, job: AgentJob) -> AgentRunResult;
}
