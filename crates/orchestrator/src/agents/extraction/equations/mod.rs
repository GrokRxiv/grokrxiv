//! `EquationCanonicalizerAgent` — Stage 5 of the multi-rep extraction
//! pipeline.
//!
//! Walks every equation in the paper (via `list_equations`) and submits a
//! canonical TeX form, MathML rendering, fuzzy dedup hash, and a short
//! semantic tag for each. Operates on the `ctx.semantic_ast` produced by the
//! deterministic Stage 2 (LaTeXML + Pandoc); falls back to scanning
//! `body.md` for `\(...\)` / `\[...\]` runs when the AST is absent (PDF-only
//! papers).
//!
//! Tool advertised to the LLM:
//!
//! - `list_equations` (agent-specific)
//! - `query_ast` (core — for follow-up drill-downs into the AST)
//! - `render_to_mathml` (agent-specific)
//! - `equation_hash` (agent-specific)
//! - `submit` (core sentinel)
//!
//! Model: `gemini-2.5-flash`. Loop budget: `max_iters=60`,
//! `max_cost_usd=0.50`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agents::extraction::{
    tools::query_ast::QueryAstTool, tools::submit::SubmitTool, ExtractionAgent, ExtractionRole,
    Tool, ToolRegistry,
};
use crate::agents::types::{ExtractionContext, ExtractionRun, ToolSpec};

pub mod tools;

pub use tools::{
    equation_hash, render_to_mathml, EquationHashTool, ListEquationsTool, RenderToMathmlTool,
};

/// Embedded schema — kept in sync with `schemas/extraction/equations.schema.json`.
const SCHEMA_JSON: &str =
    include_str!("../../../../../../schemas/extraction/equations.schema.json");

/// The Stage-5 equation canonicalisation agent.
pub struct EquationCanonicalizerAgent {
    schema: Value,
    tool_specs: Vec<ToolSpec>,
}

impl Default for EquationCanonicalizerAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl EquationCanonicalizerAgent {
    /// Build a fresh agent. The tool specs and the embedded schema are loaded
    /// once at construction time.
    pub fn new() -> Self {
        let schema: Value =
            serde_json::from_str(SCHEMA_JSON).expect("equations.schema.json must be valid JSON");
        let tool_specs = Self::build_tool_specs();
        Self { schema, tool_specs }
    }

    /// Register the agent's tools (`list_equations`, `render_to_mathml`,
    /// `equation_hash`) on top of an empty registry then append the core
    /// `query_ast` and `submit` sentinels. We deliberately DON'T include the
    /// rest of the core toolkit (e.g. `list_files`, `crossref_lookup`) — they
    /// aren't useful for equation canonicalisation and would inflate the
    /// LLM's prompt-time tool budget.
    pub fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::empty();
        r.register(Arc::new(ListEquationsTool));
        r.register(Arc::new(QueryAstTool));
        r.register(Arc::new(RenderToMathmlTool));
        r.register(Arc::new(EquationHashTool));
        r.register(Arc::new(SubmitTool));
        r
    }

    fn build_tool_specs() -> Vec<ToolSpec> {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ListEquationsTool),
            Arc::new(QueryAstTool),
            Arc::new(RenderToMathmlTool),
            Arc::new(EquationHashTool),
            Arc::new(SubmitTool),
        ];
        tools
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.schema().clone(),
            })
            .collect()
    }
}

#[async_trait]
impl ExtractionAgent for EquationCanonicalizerAgent {
    fn name(&self) -> &'static str {
        "equation_canonicalizer"
    }

    fn role(&self) -> ExtractionRole {
        ExtractionRole::EquationCanonicalizer
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_specs.clone()
    }

    fn system_prompt(&self) -> String {
        "You are normalizing the equations in this paper. Use `list_equations` to get every \
         equation (id + raw TeX + context); for each, produce a canonical TeX form \
         (`\\frac` not `\\over`, `x^{-1}` not `1/x` where unambiguous, standard operator \
         names), call `render_to_mathml` to get MathML, call `equation_hash` to deduplicate, \
         and tag each with a short semantic label (`identity` / `inequality` / `definition` \
         / `theorem-statement` / `pde` / `algebraic` / `other`). After `list_equations` \
         returns, always finish with `submit(...)`: submit all discovered equations, or submit \
         `{equations: [], reason: \"no_equations_in_paper\"}` only when the tool returns an \
         empty list. The ONLY way to finish is by calling `submit(...)` with a payload matching \
         the supplied JSON schema."
            .to_string()
    }

    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        format!(
            "Paper: {arxiv} (title: {title}). Walk every equation in this paper and submit the \
             canonical list. Start by calling list_equations(), then call submit(...) with the \
             final payload; do not stop with prose.",
            arxiv = ctx.arxiv_id,
            title = ctx.extract.title,
        )
    }

    async fn run(
        &self,
        runner: Arc<dyn crate::agents::traits::AgentRunner>,
        spec: &crate::agents::types::AgentSpec,
        ctx: ExtractionContext<'_>,
    ) -> anyhow::Result<ExtractionRun>
    where
        Self: Sized,
    {
        debug_assert!(
            ctx.max_cost_usd > 0.0,
            "ExtractionContext.max_cost_usd must be populated (FP-RPT3a A5)"
        );
        let max_iters = ctx.max_iters as usize;
        let max_cost_usd = ctx.max_cost_usd;
        crate::agents::extraction::run_tool_loop(self, runner, spec, ctx, max_iters, max_cost_usd)
            .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::agents::extraction::run_tool_loop;
    use crate::agents::traits::AgentRunner;
    use crate::agents::types::{AgentSpec, Message};
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, ToolCompletion, Usage};
    use grokrxiv_schemas::{AgentRole, PaperExtract};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Minimal scripted runner — returns queued [`ToolCompletion`]s in order.
    /// Local copy because the equivalent helper in `extraction::loop::tests`
    /// is private to that module.
    pub struct ScriptedRunner {
        queue: Mutex<Vec<ToolCompletion>>,
    }
    impl ScriptedRunner {
        pub fn new(turns: Vec<ToolCompletion>) -> Self {
            Self {
                queue: Mutex::new(turns),
            }
        }
    }
    #[async_trait]
    impl AgentRunner for ScriptedRunner {
        fn name(&self) -> &'static str {
            "scripted-equation-canon"
        }
        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &crate::agents::types::AgentInput,
        ) -> anyhow::Result<crate::agents::types::AgentRun> {
            unimplemented!("scripted runner is tool-only")
        }
        async fn complete_with_tools(
            &self,
            _spec: &AgentSpec,
            _messages: &[Message],
            _tools: &[crate::agents::types::ToolSpec],
            _ctx: &crate::agents::extraction::ToolCtx<'_>,
        ) -> anyhow::Result<ToolCompletion> {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("scripted runner queue exhausted");
            }
            Ok(q.remove(0))
        }
    }

    fn paper_extract() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2401.99999v1".into(),
            title: "Equation Test Paper".into(),
            authors: vec![],
            abstract_: "abs".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grokrxiv-eqagent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn fake_spec() -> AgentSpec {
        AgentSpec::api_default(
            AgentRole::Summary,
            "gemini".to_string(),
            "gemini-2.5-pro".to_string(),
        )
    }

    fn turn_call(name: &str, args: serde_json::Value, id: &str) -> ToolCompletion {
        ToolCompletion {
            tool_calls: vec![ProviderToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args,
            }],
            text: String::new(),
            finish_reason: FinishReason::ToolUse,
            usage: Usage::default(),
            raw: json!({}),
        }
    }
    fn turn_submit(payload: serde_json::Value) -> ToolCompletion {
        ToolCompletion {
            tool_calls: vec![ProviderToolCall {
                id: "call_submit".into(),
                name: "submit".into(),
                arguments: payload,
            }],
            text: String::new(),
            finish_reason: FinishReason::ToolUse,
            usage: Usage::default(),
            raw: json!({}),
        }
    }

    #[test]
    fn equation_canon_advertises_correct_tools() {
        let agent = EquationCanonicalizerAgent::new();
        let names: Vec<String> = agent.tools().into_iter().map(|t| t.name).collect();
        for must in [
            "list_equations",
            "query_ast",
            "render_to_mathml",
            "equation_hash",
            "submit",
        ] {
            assert!(
                names.iter().any(|n| n == must),
                "missing tool {must}: have {names:?}"
            );
        }
        assert_eq!(agent.role(), ExtractionRole::EquationCanonicalizer);
        assert_eq!(agent.name(), "equation_canonicalizer");
    }

    #[tokio::test]
    async fn equation_canon_run_via_mock_runner() {
        let agent = EquationCanonicalizerAgent::new();
        let canonical = json!({
            "equations": [{
                "id": "eq-1",
                "canonical_tex": "a+b",
                "mathml": "<math><mi>a</mi><mo>+</mo><mi>b</mi></math>",
                "semantic_tag": "algebraic",
                "hash": "deadbeef00000000"
            }],
            "reason": null
        });
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_call("list_equations", json!({}), "c1"),
            turn_call("render_to_mathml", json!({ "tex": "a+b" }), "c2"),
            turn_call("equation_hash", json!({ "canonical_tex": "a+b" }), "c3"),
            turn_submit(canonical.clone()),
        ]));
        let tmp = tempdir();
        std::fs::write(
            tmp.path().join("body.md"),
            "# Introduction\n\nWe have \\(a+b\\).\n",
        )
        .unwrap();
        // Bogus latexml binary so render_to_mathml is fast + deterministic.
        std::env::set_var("GROKRXIV_LATEXML_BIN", "__no_such_binary_grokrxiv__");
        let pe = paper_extract();
        let registry = Arc::new(EquationCanonicalizerAgent::registry());
        let ec = crate::agents::types::ExtractionContext {
            workdir: tmp.path(),
            extract: &pe,
            semantic_ast: None,
            paper_id: uuid::Uuid::nil(),
            arxiv_id: "2401.99999v1",
            registry,
            max_cost_usd: 1.0,
            max_iters: 10,
        };
        let spec = fake_spec();
        let run = run_tool_loop(&agent, runner, &spec, ec, 10, 1.0)
            .await
            .unwrap();
        std::env::remove_var("GROKRXIV_LATEXML_BIN");
        assert_eq!(run.output, canonical);
        assert!(
            run.tool_calls.iter().any(|c| c.tool == "list_equations"),
            "expected list_equations call: {:?}",
            run.tool_calls
        );
        assert!(
            run.tool_calls.iter().any(|c| c.tool == "render_to_mathml"),
            "expected render_to_mathml call: {:?}",
            run.tool_calls
        );
        assert!(
            run.tool_calls.iter().any(|c| c.tool == "equation_hash"),
            "expected equation_hash call: {:?}",
            run.tool_calls
        );
        assert!(
            run.tool_calls
                .iter()
                .find(|c| c.tool == "submit")
                .map(|c| c.ok)
                .unwrap_or(false),
            "submit should be ok: {:?}",
            run.tool_calls
        );
    }
}
