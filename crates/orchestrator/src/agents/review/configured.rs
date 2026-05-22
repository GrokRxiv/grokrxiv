//! Configured review-agent bindings.
//!
//! Role-specific behavior lives in the supervisor's input construction and in
//! the YAML/spec registry. A configured review agent is just the resolved
//! [`AgentSpec`] plus a thin dispatch method to the selected runner.

use crate::agents::types::{AgentInput, AgentRun, AgentSpec};
use crate::agents::AgentRunner;

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
    pub fn role(&self) -> &str {
        &self.spec.role
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

/// Factory retained for call-site clarity. Role selection is data in the spec,
/// so construction is no longer a six-way match.
pub fn build_agent(spec: AgentSpec) -> ConfiguredAgent {
    ConfiguredAgent::new(spec)
}
