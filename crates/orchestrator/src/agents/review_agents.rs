//! Role-level agent bindings.
//!
//! Role-specific behavior lives in the supervisor's input construction and in
//! the YAML/spec registry. A configured review agent is just the resolved
//! [`AgentSpec`] plus a thin dispatch method to the selected runner.

use super::traits::AgentRunner;
use super::types::{AgentInput, AgentRun, AgentSpec};

/// Fully resolved review agent. It intentionally has no per-role subclasses:
/// the role is data on `spec`, and every role delegates to the same runner
/// contract.
pub struct ConfiguredAgent {
    spec: AgentSpec,
}

impl ConfiguredAgent {
    /// Build with the supplied spec.
    pub fn new(spec: AgentSpec) -> Self {
        Self { spec }
    }

    /// Which review role this agent is configured for.
    pub fn role(&self) -> grokrxiv_schemas::AgentRole {
        self.spec.role
    }

    /// The fully resolved spec for this run.
    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Execute the role against the supplied input via the chosen runner.
    pub async fn run(
        &self,
        runner: &dyn AgentRunner,
        input: AgentInput,
    ) -> anyhow::Result<AgentRun> {
        runner.run(&self.spec, &input).await
    }
}

/// Render helper — synthesizes review.{html, md, tex} from the 6 review outputs.
/// Wires into the supervisor stage between `meta_reviewer` and the
/// `awaiting_moderation` status transition.
pub struct RenderAgent {
    spec: AgentSpec,
}

impl RenderAgent {
    /// Build with the supplied spec.
    pub fn new(spec: AgentSpec) -> Self {
        Self { spec }
    }

    /// Return the render spec.
    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Run the render helper through an agent runner. This is a plain helper,
    /// outside the review-agent registry, to avoid colliding with
    /// `AgentRole::MetaReviewer`.
    pub async fn run(
        &self,
        runner: &dyn AgentRunner,
        input: AgentInput,
    ) -> anyhow::Result<AgentRun> {
        // TODO(Track G integration): wire RenderAgent::run via ApiRunner to invoke
        // crates::render::render_review(...) which already produces {html, md, tex}.
        // Wrap the output as the render_artifact JSON shape.
        runner.run(&self.spec, &input).await
    }
}

/// Factory retained for call-site clarity. Role selection is data in the spec,
/// so construction is no longer a six-way match.
pub fn build_agent(spec: AgentSpec) -> ConfiguredAgent {
    ConfiguredAgent::new(spec)
}

#[cfg(test)]
mod render_agent_tests {
    //! Unit tests for `RenderAgent`. We define a local `FakeAgentRunner` here
    //! rather than depending on Track A's test fixtures so this track can land
    //! independently.
    //!
    //! These tests pin two behaviors:
    //! 1. `RenderAgent` stays outside the configured review-agent registry.
    //! 2. `RenderAgent::run()` is a thin delegate — it forwards the spec and
    //!    input to the runner and returns whatever the runner produced.
    //!    The actual `crates/render` invocation will be wired through the
    //!    `ApiRunner` in Track G integration.
    use super::*;
    use crate::agents::types::{AgentRunnerKind, SandboxPolicy};
    use async_trait::async_trait;
    use grokrxiv_schemas::AgentRole;
    use uuid::Uuid;

    /// Minimal fake runner: records the last call and returns a synthetic
    /// `AgentRun` constructed from the spec/input it was handed.
    struct FakeAgentRunner {
        /// What the runner should return as its `AgentRun.output`.
        output: serde_json::Value,
    }

    #[async_trait]
    impl AgentRunner for FakeAgentRunner {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn run(&self, spec: &AgentSpec, _input: &AgentInput) -> anyhow::Result<AgentRun> {
            Ok(AgentRun {
                role: spec.role,
                runner: AgentRunnerKind::Api,
                model: spec.model.clone(),
                output: self.output.clone(),
                verifier_status: None,
                verifier_notes: None,
                tokens_in: Some(0),
                tokens_out: Some(0),
                latency_ms: 0,
                cache_hit: false,
                sandbox_ref: None,
            })
        }
    }

    /// Build a `RenderAgent` with a spec shaped like what `agents/render_agent.yaml`
    /// would produce after config loading.
    fn render_agent_with_spec() -> RenderAgent {
        let spec = AgentSpec {
            role: AgentRole::MetaReviewer,
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            provider: "claude".to_string(),
            model: "claude-haiku-4-5-20251001".to_string(),
            schema: std::sync::Arc::new(serde_json::json!({
                "type": "object",
                "required": ["html", "md", "tex", "has_math", "macros_used", "section_count"]
            })),
            max_retries: 2,
            timeout_secs: 90,
        };
        RenderAgent::new(spec)
    }

    /// Synthetic input mimicking a post-meta-reviewer payload the render
    /// agent would be invoked with.
    fn fake_input() -> AgentInput {
        AgentInput {
            paper_id: Uuid::nil(),
            review_id: Uuid::nil(),
            role: AgentRole::MetaReviewer,
            content_hash_material: serde_json::json!({}),
            artifact: serde_json::json!({
                "summary": "s",
                "strengths": [],
                "weaknesses": [],
                "questions": [],
                "recommendation": "accept",
                "confidence": 0.9
            }),
            system_prompt: "render the review".to_string(),
            user_prompt: "meta review payload".to_string(),
            source_bundle_path: None,
        }
    }

    #[tokio::test]
    async fn test_render_agent_keeps_spec_without_review_role_identity() {
        let agent = render_agent_with_spec();
        assert_eq!(agent.spec().role, AgentRole::MetaReviewer);
    }

    #[tokio::test]
    async fn test_render_agent_delegates_to_runner() {
        let agent = render_agent_with_spec();
        let synthetic_output = serde_json::json!({
            "html": "<html><body><h2>Section</h2></body></html>",
            "md": "# Section\n",
            "tex": "\\section{Section}",
            "has_math": false,
            "macros_used": [],
            "section_count": 1
        });
        let runner = FakeAgentRunner {
            output: synthetic_output.clone(),
        };

        let run = agent
            .run(&runner, fake_input())
            .await
            .expect("fake runner should succeed");

        assert_eq!(run.role, AgentRole::MetaReviewer);
        assert_eq!(run.runner, AgentRunnerKind::Api);
        assert_eq!(run.model, "claude-haiku-4-5-20251001");
        assert_eq!(run.output, synthetic_output);
        assert!(!run.cache_hit, "delegated runs are never cache hits");
    }
}
