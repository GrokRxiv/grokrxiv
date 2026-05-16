//! Trait definitions for the agent runtime.
//!
//! - [`ReviewAgent`] is the role-level worker. There is one impl per review
//!   role (`SummaryAgent`, ..., `RenderAgent`).
//! - [`AgentRunner`] is the execution backend. There are 4 concrete impls:
//!   `ApiRunner`, `CliRunner`, `CloudRunner`, `LocalInferenceRunner`.
//!
//! The supervisor owns side effects (cache, verifier ladder, DB persist,
//! render, publish). Agents and runners are pure-ish — they reason and
//! execute; they don't touch the DB or open PRs.

use async_trait::async_trait;
use grokrxiv_schemas::AgentRole;

use super::extraction::ToolCtx;
use super::types::{AgentInput, AgentRun, AgentSpec, Message, ToolCompletion, ToolSpec};

/// Role-level worker. Owns prompt construction, schema selection, and
/// delegation to the configured runner.
#[async_trait]
pub trait ReviewAgent: Send + Sync {
    /// Which role this agent implements.
    fn role(&self) -> AgentRole;

    /// The fully resolved spec for this run (provider/model/runner/sandbox).
    fn spec(&self) -> &AgentSpec;

    /// Execute the role against the supplied input via the chosen runner.
    ///
    /// The supervisor decides whether to call this (cache miss) or short-
    /// circuit with a cached `AgentRun`.
    async fn run(
        &self,
        runner: &dyn AgentRunner,
        input: AgentInput,
    ) -> anyhow::Result<AgentRun>;
}

/// Execution backend. Receives a fully prepared spec + input and returns the
/// structured run result. Implementations:
///
/// - [`super::runners::api::ApiRunner`] — direct LLM provider API calls
/// - [`super::runners::cli::CliRunner`] — local subprocess (`claude` /
///   `codex` / `gemini` based on `spec.provider`)
/// - [`super::runners::cloud::CloudRunner`] — Vercel Open Agents / E2B
/// - [`super::runners::local_inference::LocalInferenceRunner`] — Ollama via
///   LiteLLM (preferred) or direct
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Friendly name for logs and the `doctor` preflight.
    fn name(&self) -> &'static str;

    /// Execute the call. The runner is responsible for:
    ///
    /// - issuing the LLM request / spawning the subprocess / posting to the
    ///   cloud service
    /// - one-shot corrective retry on JSON parse failure
    /// - returning a valid [`AgentRun`] with `cache_hit: false`
    ///
    /// The runner is NOT responsible for: cache lookup, verifier ladder, DB
    /// persistence — those live in the supervisor.
    async fn run(
        &self,
        spec: &AgentSpec,
        input: &AgentInput,
    ) -> anyhow::Result<AgentRun>;

    /// Run one turn of an extraction agent's tool-call loop. The runner
    /// translates `tools` into the provider's native tool format, sends the
    /// conversation, and returns any emitted tool calls plus the model's text.
    ///
    /// Default impl errors so non-tool runners fail loudly rather than
    /// silently degrading. Concrete runners override only when their backend
    /// supports tool-calling.
    async fn complete_with_tools(
        &self,
        _spec: &AgentSpec,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _ctx: &ToolCtx<'_>,
    ) -> anyhow::Result<ToolCompletion> {
        anyhow::bail!("runner `{}` does not support tools", self.name())
    }
}
