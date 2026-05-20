//! Concrete [`ReviewAgent`] implementations — one per review role.
//!
//! Each agent owns its role identity and delegates execution to the runner
//! the supervisor picks for it. The bodies are intentionally minimal because
//! all six specialist roles share the same flow (build prompt + spec, hand
//! off to runner). Role-specific behavior (e.g. `MetaReviewerAgent`'s
//! specialists-only input shape) lives in the supervisor's input
//! construction, not in the agent itself.
//!
//! `RenderAgent` is intentionally outside [`ReviewAgent`]: it runs after the
//! meta-reviewer and produces `{html, md, tex}` artifacts, so it must not share
//! the persisted `meta_reviewer` role identity.

use async_trait::async_trait;
use grokrxiv_schemas::AgentRole;

use super::traits::{AgentRunner, ReviewAgent};
use super::types::{AgentInput, AgentRun, AgentSpec};

macro_rules! review_agent_impl {
    ($name:ident, $role:expr) => {
        #[doc = concat!("`ReviewAgent` for the `", stringify!($role), "` role.")]
        pub struct $name {
            spec: AgentSpec,
        }

        impl $name {
            /// Build with the supplied spec.
            pub fn new(spec: AgentSpec) -> Self {
                Self { spec }
            }
        }

        #[async_trait]
        impl ReviewAgent for $name {
            fn role(&self) -> AgentRole {
                $role
            }

            fn spec(&self) -> &AgentSpec {
                &self.spec
            }

            async fn run(
                &self,
                runner: &dyn AgentRunner,
                input: AgentInput,
            ) -> anyhow::Result<AgentRun> {
                runner.run(&self.spec, &input).await
            }
        }
    };
}

review_agent_impl!(SummaryAgent, AgentRole::Summary);
review_agent_impl!(TechnicalCorrectnessAgent, AgentRole::TechnicalCorrectness);
review_agent_impl!(NoveltyAgent, AgentRole::Novelty);
review_agent_impl!(ReproducibilityAgent, AgentRole::Reproducibility);
review_agent_impl!(CitationAgent, AgentRole::Citation);
review_agent_impl!(MetaReviewerAgent, AgentRole::MetaReviewer);

// RenderAgent is intentionally NOT mapped to AgentRole — it runs after the
// 6 review roles and emits render artifacts, not a review JSON.

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
    /// not a `ReviewAgent`, to avoid colliding with `AgentRole::MetaReviewer`.
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

/// Factory: produce the right `ReviewAgent` impl for a role given its spec.
pub fn build_agent(spec: AgentSpec) -> Box<dyn ReviewAgent> {
    match spec.role {
        AgentRole::Summary => Box::new(SummaryAgent::new(spec)),
        AgentRole::TechnicalCorrectness => Box::new(TechnicalCorrectnessAgent::new(spec)),
        AgentRole::Novelty => Box::new(NoveltyAgent::new(spec)),
        AgentRole::Reproducibility => Box::new(ReproducibilityAgent::new(spec)),
        AgentRole::Citation => Box::new(CitationAgent::new(spec)),
        AgentRole::MetaReviewer => Box::new(MetaReviewerAgent::new(spec)),
    }
}

#[cfg(test)]
mod render_agent_tests {
    //! Unit tests for `RenderAgent`. We define a local `FakeAgentRunner` here
    //! rather than depending on Track A's test fixtures so this track can land
    //! independently.
    //!
    //! These tests pin two behaviors:
    //! 1. `RenderAgent` does not implement `ReviewAgent` and therefore cannot
    //!    collide with `AgentRole::MetaReviewer`.
    //! 2. `RenderAgent::run()` is a thin delegate — it forwards the spec and
    //!    input to the runner and returns whatever the runner produced.
    //!    The actual `crates/render` invocation will be wired through the
    //!    `ApiRunner` in Track G integration.
    use super::*;
    use crate::agents::types::{AgentMode, SandboxPolicy};
    use crate::agents::types::{AgentRunnerKind, ToolPolicy};
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
            mode: AgentMode::ReviewOnly,
            provider: "claude".to_string(),
            model: "claude-haiku-4-5-20251001".to_string(),
            schema: std::sync::Arc::new(serde_json::json!({
                "type": "object",
                "required": ["html", "md", "tex", "has_math", "macros_used", "section_count"]
            })),
            tool_policy: ToolPolicy::default(),
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
