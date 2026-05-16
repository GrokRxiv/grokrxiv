//! `TheoremGraphExtractorAgent` (Stage 6) — builds a dependency graph over
//! theorem-like blocks (theorem / lemma / proposition / corollary / definition
//! / proof / remark / example) and resolves every `\ref{}` edge.
//!
//! The agent is **tool-using**: it iteratively calls `list_sections`,
//! `read_section`, `query_ast`, and `resolve_label` to navigate the paper,
//! then submits the full graph via `submit`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agents::extraction::{
    ExtractionAgent, ExtractionRole, ToolRegistry, ToolSpec,
};
use crate::agents::types::{AgentSpec, ExtractionContext, ExtractionRun};
use crate::agents::traits::AgentRunner;

pub mod tools;

/// Agent name (matches the role's snake-case identifier).
pub const NAME: &str = "theorem_graph_extractor";

/// Bytes of the output schema, embedded so the agent doesn't need to read
/// from disk at runtime.
const SCHEMA_JSON: &str =
    include_str!("../../../../../../schemas/extraction/theorems.schema.json");

/// Concrete agent. Wraps an embedded output schema + a per-agent
/// [`ToolRegistry`] populated with both the core toolkit and the three
/// theorem-specific tools.
pub struct TheoremGraphExtractorAgent {
    schema: Value,
    tool_specs: Vec<ToolSpec>,
    /// Per-run loop ceilings. Pulled from the Track-8f plan: 50 iters,
    /// $0.08 cost ceiling.
    max_iters: usize,
    max_cost_usd: f32,
}

impl Default for TheoremGraphExtractorAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl TheoremGraphExtractorAgent {
    /// Build the agent with its default registry + cost ceilings.
    pub fn new() -> Self {
        let schema: Value = serde_json::from_str(SCHEMA_JSON)
            .expect("theorems.schema.json must be valid JSON");
        let registry = Self::build_registry();
        let tool_specs = registry.specs();
        Self {
            schema,
            tool_specs,
            max_iters: 50,
            max_cost_usd: 0.80,
        }
    }

    /// Construct an [`Arc<ToolRegistry>`] suitable for placing inside an
    /// [`ExtractionContext`] when running this agent. Includes the shared core
    /// toolkit plus the three theorem-specific tools.
    pub fn build_registry() -> ToolRegistry {
        let mut r = ToolRegistry::with_core_tools();
        r.register(Arc::new(tools::ListSectionsTool));
        r.register(Arc::new(tools::ReadSectionTool));
        r.register(Arc::new(tools::ResolveLabelTool));
        r
    }
}

#[async_trait]
impl ExtractionAgent for TheoremGraphExtractorAgent {
    fn name(&self) -> &'static str {
        NAME
    }
    fn role(&self) -> ExtractionRole {
        ExtractionRole::TheoremGraphExtractor
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_specs.clone()
    }
    fn system_prompt(&self) -> String {
        "You are building the theorem dependency graph for this paper. \
         Use `list_sections` to see structure, `read_section` to inspect specific \
         sections, and `query_ast` to find theorem-like environments. For every \
         theorem/lemma/proposition/corollary/definition/proof block: extract its \
         id, type, statement (first paragraph), and the section it lives in. \
         Resolve every `\\ref{}` in proofs via `resolve_label` to build the \
         `depends_on` edges. Submit the full graph by calling `submit` with a \
         payload matching the schema. Do NOT emit prose; the ONLY way to finish \
         is by calling `submit`."
            .to_string()
    }
    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        format!(
            "Paper: {arxiv} (title: {title}). The unpacked source bundle is at \
             ./ (workdir; `body.md` is the rendered markdown). Start by calling \
             `list_sections` to map the document, then read sections that contain \
             theorem-like blocks. For each theorem/lemma/proposition/corollary/\
             definition/proof you find, record `id`, `type`, `statement` (first \
             paragraph or 1-2 sentences), and `section`. For every `\\ref{{...}}` \
             you see inside a proof, call `resolve_label` and add the resolved id \
             to that proof's `depends_on` list. Finally call `submit` with the \
             entire `theorem_graph`.",
            arxiv = ctx.arxiv_id,
            title = ctx.extract.title,
        )
    }
    async fn run(
        &self,
        runner: Arc<dyn AgentRunner>,
        spec: &AgentSpec,
        ctx: ExtractionContext<'_>,
    ) -> anyhow::Result<ExtractionRun>
    where
        Self: Sized,
    {
        crate::agents::extraction::run_tool_loop(
            self,
            runner,
            spec,
            ctx,
            self.max_iters,
            self.max_cost_usd,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::extraction::r#loop::run_tool_loop;
    use crate::agents::extraction::ToolCtx;
    use crate::agents::traits::AgentRunner;
    use crate::agents::types::{AgentSpec, ExtractionContext, Message};
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{
        FinishReason, ProviderToolCall, ToolCompletion, ToolSpec as LlmToolSpec, Usage,
    };
    use grokrxiv_schemas::{AgentRole, PaperExtract};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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
            "grokrxiv-theorem-agent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn paper_extract() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2401.99999v1".into(),
            title: "Toy Theorem Paper".into(),
            authors: vec![],
            abstract_: "abs".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    fn fake_spec() -> AgentSpec {
        AgentSpec::api_default(
            AgentRole::Summary,
            "claude".to_string(),
            "claude-test".to_string(),
        )
    }

    /// Scripted runner — hands back canned tool-call turns in order. Mirrors
    /// the runner used in the framework's loop tests but lives here so the
    /// agent-level test is self-contained.
    struct ScriptedRunner {
        queue: Mutex<Vec<ToolCompletion>>,
        seen_tools: Mutex<Vec<Vec<String>>>,
    }
    impl ScriptedRunner {
        fn new(turns: Vec<ToolCompletion>) -> Self {
            Self {
                queue: Mutex::new(turns),
                seen_tools: Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait]
    impl AgentRunner for ScriptedRunner {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &crate::agents::types::AgentInput,
        ) -> anyhow::Result<crate::agents::types::AgentRun> {
            unimplemented!("tool-only");
        }
        async fn complete_with_tools(
            &self,
            _spec: &AgentSpec,
            _messages: &[Message],
            tools: &[LlmToolSpec],
            _ctx: &ToolCtx<'_>,
        ) -> anyhow::Result<ToolCompletion> {
            self.seen_tools
                .lock()
                .unwrap()
                .push(tools.iter().map(|t| t.name.clone()).collect());
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("scripted runner queue exhausted");
            }
            Ok(q.remove(0))
        }
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
    fn theorem_graph_advertises_correct_tools() {
        let agent = TheoremGraphExtractorAgent::new();
        let names: Vec<String> = agent.tools().into_iter().map(|t| t.name).collect();
        // Required: read_file, query_ast, list_sections, read_section,
        // resolve_label, submit. Plus the core toolkit also provides
        // list_files, crossref_lookup, arxiv_lookup — that's fine.
        for required in &[
            "read_file",
            "query_ast",
            "list_sections",
            "read_section",
            "resolve_label",
            "submit",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing required tool `{required}` in {names:?}"
            );
        }
    }

    #[test]
    fn role_is_theorem_graph_extractor() {
        let a = TheoremGraphExtractorAgent::new();
        assert_eq!(a.role(), ExtractionRole::TheoremGraphExtractor);
        assert_eq!(a.name(), "theorem_graph_extractor");
    }

    #[test]
    fn schema_is_valid_and_constrains_output() {
        let a = TheoremGraphExtractorAgent::new();
        let schema = a.schema();
        assert!(schema.get("properties").is_some());
        let validator = jsonschema::validator_for(schema).expect("schema compiles");
        let good = json!({
            "theorem_graph": [
                {"id":"T1","type":"theorem","statement":"X.","section":"sec-2","depends_on":[]}
            ]
        });
        assert!(validator.validate(&good).is_ok());
        // Missing depends_on -> invalid.
        let bad = json!({
            "theorem_graph": [
                {"id":"T1","type":"theorem","statement":"X.","section":"sec-2"}
            ]
        });
        assert!(validator.validate(&bad).is_err());
    }

    #[tokio::test]
    async fn theorem_graph_run_via_mock_runner() {
        let agent = TheoremGraphExtractorAgent::new();
        let dir = tempdir();
        // A toy markdown body with two sections, one theorem and one proof
        // that references the theorem.
        let body = concat!(
            "# Intro\n\nbackground\n\n",
            "# Main Results\n\n",
            "\\begin{theorem}\\label{thm:foo} Let X be Hausdorff. \\end{theorem}\n\n",
            "\\begin{proof}\\label{prf:foo} By \\ref{thm:foo}, we have... \\end{proof}\n",
        );
        std::fs::write(dir.path().join("body.md"), body).unwrap();

        let pe = paper_extract();
        let registry = Arc::new(TheoremGraphExtractorAgent::build_registry());
        let ec = ExtractionContext {
            workdir: dir.path(),
            extract: &pe,
            semantic_ast: None,
            paper_id: uuid::Uuid::nil(),
            arxiv_id: "2401.99999v1",
            registry,
        };

        let submit_payload = json!({
            "theorem_graph": [
                {
                    "id": "T1",
                    "type": "theorem",
                    "statement": "Let X be Hausdorff.",
                    "section": "sec-2",
                    "depends_on": []
                },
                {
                    "id": "P1",
                    "type": "proof",
                    "statement": "By Theorem T1, we have...",
                    "section": "sec-2",
                    "depends_on": ["thm:foo"]
                }
            ]
        });

        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_call("list_sections", json!({}), "c1"),
            turn_call("read_section", json!({"id":"sec-2"}), "c2"),
            turn_call("resolve_label", json!({"label":"thm:foo"}), "c3"),
            turn_submit(submit_payload.clone()),
        ]));

        let spec = fake_spec();
        let run = run_tool_loop(&agent, runner, &spec, ec, 10, 1.0)
            .await
            .expect("runs to completion");
        assert_eq!(run.output, submit_payload);
        let tg = run.output["theorem_graph"].as_array().unwrap();
        assert!(!tg.is_empty(), "theorem_graph should not be empty");
        let has_edge = tg
            .iter()
            .any(|e| !e["depends_on"].as_array().unwrap().is_empty());
        assert!(has_edge, "expected at least one depends_on edge");

        // Spot-check the audit log: each non-submit call should have come back
        // OK and we should see our three tools in the order issued.
        let tool_sequence: Vec<&str> = run
            .tool_calls
            .iter()
            .map(|c| c.tool.as_str())
            .collect();
        assert_eq!(
            tool_sequence,
            vec!["list_sections", "read_section", "resolve_label", "submit"]
        );
    }
}
