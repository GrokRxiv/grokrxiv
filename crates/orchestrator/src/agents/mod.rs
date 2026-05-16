//! Agent runtime: `ReviewAgent` + `AgentRunner` + 4 runner backends.
//!
//! See `research/agent-runner.md` for the design and
//! `~/.claude/plans/rpt2-real-agent-runtime.md` for the implementation plan.
//!
//! Layout:
//! - [`types`] — taxonomy enums + `AgentSpec` / `AgentInput` / `AgentRun`
//! - [`traits`] — `ReviewAgent` + `AgentRunner` async traits
//! - [`review_agents`] — 7 concrete `ReviewAgent` impls (6 review + 1 render)
//! - [`runners`] — 4 backend impls (`api`, `cli`, `cloud`, `local_inference`)
//! - [`sandbox`] — orthogonal `SandboxPolicy::Container` helper

pub mod review_agents;
pub mod runners;
pub mod sandbox;
pub mod traits;
pub mod types;

pub use review_agents::{
    build_agent, CitationAgent, MetaReviewerAgent, NoveltyAgent, RenderAgent,
    ReproducibilityAgent, SummaryAgent, TechnicalCorrectnessAgent,
};
pub use traits::{AgentRunner, ReviewAgent};
pub use types::{
    AgentInput, AgentMode, AgentRun, AgentRunnerKind, AgentSpec, RevisionTarget, RoleSpecMap,
    SandboxPolicy, ToolPolicy,
};

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use grokrxiv_schemas::AgentRole;
    use serde_json::json;
    use uuid::Uuid;

    use super::traits::{AgentRunner, ReviewAgent};
    use super::types::{
        AgentInput, AgentMode, AgentRun, AgentRunnerKind, AgentSpec, SandboxPolicy, ToolPolicy,
    };
    use super::SummaryAgent;

    /// Test double that hands back a synthetic [`AgentRun`] without making
    /// any LLM calls. Verifies the [`ReviewAgent::run`] propagation contract:
    /// the agent's role + spec are used verbatim, and the runner's output is
    /// returned unchanged.
    struct FakeAgentRunner {
        canned: AgentRun,
    }

    #[async_trait]
    impl AgentRunner for FakeAgentRunner {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &AgentInput,
        ) -> anyhow::Result<AgentRun> {
            Ok(self.canned.clone())
        }
    }

    fn fake_spec() -> AgentSpec {
        AgentSpec {
            role: AgentRole::Summary,
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            mode: AgentMode::ReviewOnly,
            provider: "claude".to_string(),
            model: "fake-model".to_string(),
            schema: json!({ "type": "object" }),
            tool_policy: ToolPolicy::default(),
            max_retries: 2,
            timeout_secs: 30,
        }
    }

    fn fake_input(role: AgentRole) -> AgentInput {
        let artifact = json!({ "hello": "world" });
        AgentInput {
            paper_id: Uuid::new_v4(),
            review_id: Uuid::new_v4(),
            role,
            content_hash_material: artifact.clone(),
            artifact,
            system_prompt: "test-system".to_string(),
            user_prompt: "test-user".to_string(),
            source_bundle_path: None,
        }
    }

    #[tokio::test]
    async fn summary_agent_propagates_runner_output() {
        let spec = fake_spec();
        let agent = SummaryAgent::new(spec.clone());
        let canned = AgentRun {
            role: AgentRole::Summary,
            runner: AgentRunnerKind::Api,
            model: spec.model.clone(),
            output: json!({ "tldr": "ok" }),
            verifier_status: None,
            verifier_notes: None,
            tokens_in: Some(42),
            tokens_out: Some(17),
            latency_ms: 123,
            cache_hit: false,
            sandbox_ref: None,
        };
        let runner = FakeAgentRunner {
            canned: canned.clone(),
        };

        let run = agent
            .run(&runner, fake_input(AgentRole::Summary))
            .await
            .expect("fake runner succeeds");

        assert_eq!(agent.role(), AgentRole::Summary);
        assert_eq!(run.role, canned.role);
        assert_eq!(run.runner, canned.runner);
        assert_eq!(run.model, canned.model);
        assert_eq!(run.output, canned.output);
        assert_eq!(run.tokens_in, canned.tokens_in);
        assert_eq!(run.tokens_out, canned.tokens_out);
        assert_eq!(run.latency_ms, canned.latency_ms);
        assert!(!run.cache_hit);
        assert!(run.sandbox_ref.is_none());
    }
}
