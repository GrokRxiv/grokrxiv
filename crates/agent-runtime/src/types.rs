//! Public types for the agent runtime.
//!
//! The taxonomy:
//! - [`AgentRunnerKind`]: how a role's work is executed.
//! - [`SandboxPolicy`]: orthogonal isolation policy applied to a runner.
//! - [`AgentMode`]: review-only vs revision-capable.
//! - [`RevisionTarget`]: when revising, what to patch.
//! - [`AgentSpec`]: per-role config (provider, model, runner, schema, ...).
//! - [`AgentInput`]: the payload a runner receives.
//! - [`AgentRun`]: structured output from a single runner execution.
//!
//! Historical design notes live with the GrokRxiv app docs, not in the
//! platform runtime contract.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use agenthero_llm_adapter::{
    ProviderToolCall as ToolCall, ToolChatRequest, ToolCompletion, ToolContent, ToolMessage,
    ToolSpec,
};

/// Shorthand for one message in a tool-using conversation. The shape is
/// identical to [`agenthero_llm_adapter::ToolMessage`]; this alias keeps the
/// orchestrator's call sites tidy.
pub type Message = ToolMessage;

/// Shared JSON schema document for an agent role.
pub type AgentSchema = Arc<serde_json::Value>;

/// Which execution backend handles this role's work.
///
/// Concrete sub-providers, such as which CLI binary to spawn, are selected by
/// environment variables or by the role's existing `provider:` field in
/// `agents/*.yaml` — not by this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum AgentRunnerKind {
    /// Direct provider API call. Use explicitly with `--runner api`.
    Api,
    /// Local CLI subprocess (`claude` / `codex` / `gemini`). Default for
    /// review roles. The role's
    /// `provider:` field in YAML drives which binary is spawned.
    Cli,
}

impl Default for AgentRunnerKind {
    fn default() -> Self {
        Self::Cli
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AgentInput, AgentRunnerKind};

    #[test]
    fn runner_kind_only_accepts_active_backends() {
        assert_eq!(
            serde_json::from_str::<AgentRunnerKind>(r#""api""#).unwrap(),
            AgentRunnerKind::Api
        );
        assert_eq!(
            serde_json::from_str::<AgentRunnerKind>(r#""cli""#).unwrap(),
            AgentRunnerKind::Cli
        );

        for stale in [r#""cloud""#, r#""local_inference""#] {
            assert!(
                serde_json::from_str::<AgentRunnerKind>(stale).is_err(),
                "{stale} must not deserialize as an active runner backend"
            );
        }
    }

    #[test]
    fn agent_input_contract_is_domain_neutral() {
        let input = AgentInput {
            context: BTreeMap::from([("app_key".to_string(), serde_json::json!("value"))]),
            role: "generic_agent".to_string(),
            content_hash_material: serde_json::json!({"input": true}),
            artifact: serde_json::json!({"input": true}),
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            source_bundle_path: None,
        };

        assert_eq!(
            input.context.get("app_key"),
            Some(&serde_json::json!("value"))
        );
    }
}

/// Orthogonal isolation policy. Applied UNDER runner kinds that want
/// container isolation.
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
    /// Revise the app's own generated artifact (self-improvement loop).
    AppReviewOutput,
}

impl Default for RevisionTarget {
    fn default() -> Self {
        Self::PaperLatex
    }
}

/// Per-role configuration assembled from `agents/<dag>/<role>.yaml` plus any TOML
/// profile overrides and CLI flag overrides.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    /// DAG-scoped role id, e.g. `summary` or `type_theory_validator`.
    pub role: String,
    /// Which backend executes the role.
    pub runner: AgentRunnerKind,
    /// Isolation policy applied to the runner.
    pub sandbox: SandboxPolicy,
    /// Provider name from `agents/*.yaml` (`claude` / `openai` / `gemini` /
    /// `deepseek` / etc.). Used by `ApiRunner` to dispatch to an `LLMProvider`
    /// and by `CliRunner` to pick a binary.
    pub provider: String,
    /// Model identifier (e.g. `claude-opus-4-7`, `qwen2.5:32b-instruct-q4_K_M`).
    pub model: String,
    /// Compiled output JSON schema this role must satisfy.
    pub schema: AgentSchema,
    /// Maximum corrective retries on parse/validation failure. Default 2.
    pub max_retries: u8,
    /// Hard timeout for a single runner call.
    pub timeout_secs: u32,
}

impl AgentSpec {
    /// Convenience for tests / Phase-1 wiring: a minimal spec defaulting to
    /// the API runner.
    pub fn api_default(role: impl Into<String>, provider: String, model: String) -> Self {
        Self {
            role: role.into(),
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            provider,
            model,
            schema: Arc::new(serde_json::json!({})),
            max_retries: 2,
            timeout_secs: 180,
        }
    }
}

/// Input payload a runner receives.
#[derive(Debug, Clone)]
pub struct AgentInput {
    /// App-owned execution context values. Product apps can store durable ids,
    /// tenant metadata, or other runner-scoped values here without extending
    /// the platform runtime contract.
    pub context: std::collections::BTreeMap<String, serde_json::Value>,
    /// DAG-scoped role id being executed.
    pub role: String,
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
    /// DAG-scoped role id that produced the run.
    pub role: String,
    /// Runner that executed it.
    pub runner: AgentRunnerKind,
    /// Model id reported by the runner (may differ from `spec.model` if a
    /// gateway like LiteLLM remapped it).
    pub model: String,
    /// JSON output validated against the role's schema.
    pub output: serde_json::Value,
    /// Optional verifier result if the runner ran its own verifier rungs.
    pub verifier_status: Option<String>,
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
        role: impl Into<String>,
        runner: AgentRunnerKind,
        model: String,
        output: serde_json::Value,
        tokens_in: Option<i32>,
        tokens_out: Option<i32>,
    ) -> Self {
        Self {
            role: role.into(),
            runner,
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
pub type RoleSpecMap = HashMap<String, AgentSpec>;
