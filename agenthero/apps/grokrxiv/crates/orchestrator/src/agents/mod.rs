//! Agent runtime: review bindings, extraction agents, and runner backends.
//!
//! Historical design notes live under `agenthero/apps/grokrxiv/docs/`; runtime
//! behavior is defined by app YAML and strict schema contracts.
//!
//! Layout:
//! - [`types`] — taxonomy enums + `AgentSpec` / `AgentInput` / `AgentRun`
//! - [`review`] — configured review roles, render helper, and review facts
//! - [`runners`] — `AgentRunner` plus 4 backend impls (`api`, `cli`, `cloud`,
//!   `local_inference`)
//! - [`sandbox`] — orthogonal `SandboxPolicy::Container` helper

pub mod config;
pub mod review;
pub mod runners;
pub mod sandbox;
pub mod types;

pub use review::{build_agent, ConfiguredAgent};
pub use runners::AgentRunner;
pub use types::{
    AgentInput, AgentMode, AgentRun, AgentRunnerKind, AgentSchema, AgentSpec, RevisionTarget,
    RoleSpecMap, SandboxPolicy,
};

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;
    use uuid::Uuid;

    use super::types::{AgentInput, AgentRun, AgentRunnerKind, AgentSpec, SandboxPolicy};
    use super::AgentRunner;
    use super::ConfiguredAgent;

    /// Test double that hands back a synthetic [`AgentRun`] without making
    /// any LLM calls. Verifies the [`ConfiguredAgent::run`] propagation contract:
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

        async fn run(&self, _spec: &AgentSpec, _input: &AgentInput) -> anyhow::Result<AgentRun> {
            Ok(self.canned.clone())
        }
    }

    fn fake_spec() -> AgentSpec {
        AgentSpec {
            role: "summary".to_string(),
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            provider: "claude".to_string(),
            model: "fake-model".to_string(),
            schema: std::sync::Arc::new(json!({ "type": "object" })),
            max_retries: 2,
            timeout_secs: 30,
        }
    }

    fn fake_input(role: &str) -> AgentInput {
        let artifact = json!({
            "arxiv_id": "2605.00000",
            "title": "Synthetic Agent Runtime Fixture",
            "sections": [],
            "bibliography": []
        });
        AgentInput {
            paper_id: Uuid::new_v4(),
            review_id: Uuid::new_v4(),
            role: role.to_string(),
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
        let agent = ConfiguredAgent::new(spec.clone());
        let canned = AgentRun {
            role: "summary".to_string(),
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
            .run(&runner, fake_input("summary"))
            .await
            .expect("fake runner succeeds");

        assert_eq!(agent.role(), "summary");
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

    #[test]
    fn cache_hit_run_preserves_original_runner() {
        let run = AgentRun::from_cache(
            "summary",
            AgentRunnerKind::Cli,
            "claude-sonnet-4".to_string(),
            json!({ "tldr": "cached" }),
            Some(11),
            Some(7),
        );

        assert_eq!(run.runner, AgentRunnerKind::Cli);
        assert_eq!(run.model, "claude-sonnet-4");
        assert!(run.cache_hit);
    }
}
