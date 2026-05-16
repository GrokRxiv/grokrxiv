//! Public types for the agent runtime.
//!
//! The taxonomy:
//! - [`AgentRunnerKind`]: how a role's work is executed (4 backends).
//! - [`SandboxPolicy`]: orthogonal isolation policy applied to a runner.
//! - [`AgentMode`]: review-only vs revision-capable.
//! - [`RevisionTarget`]: when revising, what to patch.
//! - [`AgentSpec`]: per-role config (provider, model, runner, sandbox, ...).
//! - [`AgentInput`]: the payload a runner receives.
//! - [`AgentRun`]: structured output from a single runner execution.
//!
//! See `research/agent-runner.md` for the full design rationale.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use grokrxiv_schemas::{AgentRole, PaperExtract, VerifierStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use grokrxiv_llm_adapter::{
    ProviderToolCall as ToolCall, ToolChatRequest, ToolCompletion, ToolContent, ToolMessage,
    ToolSpec,
};

/// Shorthand for one message in a tool-using conversation. The shape is
/// identical to [`grokrxiv_llm_adapter::ToolMessage`]; this alias keeps the
/// orchestrator's call sites tidy.
pub type Message = ToolMessage;

use crate::agents::extraction::ToolRegistry;

/// Context handed to the tool-call loop. Borrows the workdir, paper extract,
/// and optional semantic AST so tool implementations can read them without
/// cloning.
pub struct ExtractionContext<'a> {
    /// Working directory rooted at the unpacked paper source bundle. Tools
    /// like `list_files` and `read_file` are scoped to this.
    pub workdir: &'a Path,
    /// The paper extract (sections, bibliography, figures, ...).
    pub extract: &'a PaperExtract,
    /// LaTeXML-derived semantic AST; populated when the deterministic Stage 2
    /// succeeded. Drives `query_ast`.
    pub semantic_ast: Option<&'a serde_json::Value>,
    /// DB UUID of the paper this extraction is running against.
    pub paper_id: Uuid,
    /// arXiv identifier (version-suffixed) for tools that need to look up
    /// metadata.
    pub arxiv_id: &'a str,
    /// Toolkit available this run.
    pub registry: Arc<ToolRegistry>,
    /// Per-stage dollar ceiling resolved from `agents/extraction/<stage>.yaml`
    /// (FP-RPT3a A5). Each agent's `run()` forwards this to
    /// `run_tool_loop` instead of hardcoding the budget inline.
    pub max_cost_usd: f32,
    /// Per-stage iteration ceiling resolved from
    /// `agents/extraction/<stage>.yaml` (FP-RPT3a A5).
    pub max_iters: u32,
}

/// One audit-log record per tool call inside a tool-call loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Iteration number (zero-based) the call occurred on.
    pub iter: u32,
    /// Tool name (matches a `ToolSpec.name`).
    pub tool: String,
    /// Arguments the model passed to the tool.
    pub arguments: serde_json::Value,
    /// Tool result that came back. For `submit` this is the validated payload;
    /// for failures this is the error string under a `"_error"` key.
    pub result: serde_json::Value,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Wall-clock duration of the call in milliseconds.
    pub latency_ms: i64,
}

/// Result of running an extraction agent end-to-end through the tool-call loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionRun {
    /// Final validated `submit(...)` payload.
    pub output: serde_json::Value,
    /// Audit log of every tool call (including the final `submit`).
    pub tool_calls: Vec<ToolCallRecord>,
    /// Rough USD cost across all turns (best-effort; 0.0 when the runner
    /// didn't surface usage).
    pub cost_usd: f32,
    /// Wall-clock latency end-to-end in milliseconds.
    pub latency_ms: i64,
    /// Number of model turns consumed (not the number of tool calls).
    pub iters: u32,
}

/// Which execution backend handles this role's work.
///
/// Concrete sub-providers (which CLI binary; which cloud service; which OSS
/// inference server) are selected by environment variables or by the role's
/// existing `provider:` field in `agents/*.yaml` — not by this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum AgentRunnerKind {
    /// Direct provider API call. Default for all 6 review roles.
    Api,
    /// Local CLI subprocess (`claude` / `codex` / `gemini`). The role's
    /// `provider:` field in YAML drives which binary is spawned.
    Cli,
    /// Cloud agent backend (Vercel Open Agents primary; E2B alternate).
    /// `GROKRXIV_CLOUD_PROVIDER` selects.
    Cloud,
    /// Local OSS inference (Ollama via direct URL or LiteLLM gateway).
    /// `GROKRXIV_LITELLM_URL` (preferred) or `OLLAMA_HOST` (fallback) selects
    /// the endpoint.
    LocalInference,
}

impl Default for AgentRunnerKind {
    fn default() -> Self {
        Self::Api
    }
}

/// Orthogonal isolation policy. Applied UNDER any runner kind that wants
/// container isolation (typically `Cli` or `LocalInference`).
///
/// `Cloud` runners are inherently sandboxed by the cloud provider and ignore
/// this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum SandboxPolicy {
    /// Run on host. Default.
    None,
    /// Wrap the runner in a multi-arch Docker container with RO-mounted
    /// CLI auth and a per-run scratch workdir.
    Container,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::None
    }
}

/// Whether the agent only emits a review, or also emits revision patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum AgentMode {
    /// Only emit JSON review output. Today's default.
    ReviewOnly,
    /// Emit JSON review output AND a `revision_artifact` describing
    /// proposed patches.
    ReviewAndRevise,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::ReviewOnly
    }
}

/// Where revisions land when `AgentMode::ReviewAndRevise` is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum RevisionTarget {
    /// Patch the paper's own LaTeX source (the agustif-style review-author
    /// loop).
    PaperLatex,
    /// Revise GrokRxiv's own generated review artifact (self-improvement loop).
    GrokrxivReviewOutput,
}

impl Default for RevisionTarget {
    fn default() -> Self {
        Self::PaperLatex
    }
}

/// Tool-permission policy (Phase 4+ scope). Empty in RPT2; expanded later.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// Names of tools the agent may use (e.g. `read`, `grep`, `bash`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// Per-role configuration assembled from `agents/<role>.yaml` plus any TOML
/// profile overrides and CLI flag overrides.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    /// Which review role this spec is for.
    pub role: AgentRole,
    /// Which backend executes the role.
    pub runner: AgentRunnerKind,
    /// Isolation policy applied to the runner.
    pub sandbox: SandboxPolicy,
    /// Review-only or review-and-revise.
    pub mode: AgentMode,
    /// Provider name from `agents/*.yaml` (`claude` / `openai` / `gemini` /
    /// `deepseek` / etc.). Used by `ApiRunner` to dispatch to an `LLMProvider`
    /// and by `CliRunner` to pick a binary.
    pub provider: String,
    /// Model identifier (e.g. `claude-opus-4-7`, `qwen2.5:32b-instruct-q4_K_M`).
    pub model: String,
    /// Compiled output JSON schema this role must satisfy.
    pub schema: serde_json::Value,
    /// Tool permissions (Phase 4+).
    pub tool_policy: ToolPolicy,
    /// Maximum corrective retries on parse/validation failure. Default 2.
    pub max_retries: u8,
    /// Hard timeout for a single runner call.
    pub timeout_secs: u32,
}

impl AgentSpec {
    /// Convenience for tests / Phase-1 wiring: a minimal spec defaulting to
    /// the API runner.
    pub fn api_default(role: AgentRole, provider: String, model: String) -> Self {
        Self {
            role,
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            mode: AgentMode::ReviewOnly,
            provider,
            model,
            schema: serde_json::json!({}),
            tool_policy: ToolPolicy::default(),
            max_retries: 2,
            timeout_secs: 180,
        }
    }
}

/// Input payload a runner receives.
#[derive(Debug, Clone)]
pub struct AgentInput {
    /// Paper this review pertains to.
    pub paper_id: Uuid,
    /// Review ID the run belongs to.
    pub review_id: Uuid,
    /// Role being executed.
    pub role: AgentRole,
    /// Bytes used to derive the cache content hash. Typically the JSON of the
    /// upstream artifact (paper extract for specialists; specialists bundle
    /// for meta-reviewer).
    pub content_hash_material: serde_json::Value,
    /// The artifact the agent should reason over (same as
    /// `content_hash_material` for specialists; specialists-only map for
    /// meta-reviewer).
    pub artifact: serde_json::Value,
    /// Fully rendered system prompt.
    pub system_prompt: String,
    /// Fully rendered user prompt.
    pub user_prompt: String,
    /// Optional path to the paper's LaTeX source bundle for tool-using runners.
    pub source_bundle_path: Option<String>,
}

/// Structured output from one runner execution.
#[derive(Debug, Clone)]
pub struct AgentRun {
    /// Role that produced the run.
    pub role: AgentRole,
    /// Runner that executed it.
    pub runner: AgentRunnerKind,
    /// Model id reported by the runner (may differ from `spec.model` if a
    /// gateway like LiteLLM remapped it).
    pub model: String,
    /// JSON output validated against the role's schema.
    pub output: serde_json::Value,
    /// Optional verifier result if the runner ran its own verifier rungs.
    /// Today this is None — verification happens in the supervisor.
    pub verifier_status: Option<VerifierStatus>,
    /// Optional verifier notes if the runner produced them.
    pub verifier_notes: Option<serde_json::Value>,
    /// Tokens consumed on input (None if the runner doesn't account for it).
    pub tokens_in: Option<i32>,
    /// Tokens emitted as output (None if the runner doesn't account for it).
    pub tokens_out: Option<i32>,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: i32,
    /// Whether this run was served from the review cache rather than the
    /// runner. Set by the supervisor on cache hit; runners always return
    /// `false`.
    pub cache_hit: bool,
    /// Optional sandbox reference for audit (e.g. an E2B sandbox id or a
    /// container id).
    pub sandbox_ref: Option<String>,
}

impl AgentRun {
    /// Cache-hit constructor used by the supervisor when the cache short-circuits
    /// the runner call.
    pub fn from_cache(
        role: AgentRole,
        model: String,
        output: serde_json::Value,
        tokens_in: Option<i32>,
        tokens_out: Option<i32>,
    ) -> Self {
        Self {
            role,
            runner: AgentRunnerKind::Api,
            model,
            output,
            verifier_status: None,
            verifier_notes: None,
            tokens_in,
            tokens_out,
            latency_ms: 0,
            cache_hit: true,
            sandbox_ref: None,
        }
    }
}

/// Convenience type for per-role spec overrides assembled from layered config.
pub type RoleSpecMap = HashMap<AgentRole, AgentSpec>;
