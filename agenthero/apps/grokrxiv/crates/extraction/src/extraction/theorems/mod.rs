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

use crate::extraction::{ExtractionAgent, ExtractionRole, ToolRegistry, ToolSpec};
use crate::extraction::{ExtractionContext, ExtractionRun};
use agenthero_agent_runtime::AgentRunner;
use agenthero_agent_runtime::AgentSpec;

pub mod tools;

/// Agent name (matches the role's snake-case identifier).
pub const NAME: &str = "theorem_graph_extractor";

/// Bytes of the output schema, embedded so the agent doesn't need to read
/// from disk at runtime.
const SCHEMA_JSON: &str = include_str!("../../../../../schemas/extraction/theorems.schema.json");

/// Concrete agent. Wraps an embedded output schema + a per-agent
/// [`ToolRegistry`] populated with both the core toolkit and the three
/// theorem-specific tools.
pub struct TheoremGraphExtractorAgent {
    schema: Value,
    tool_specs: Vec<ToolSpec>,
}

impl Default for TheoremGraphExtractorAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl TheoremGraphExtractorAgent {
    /// Build the agent with its default registry + cost ceilings.
    pub fn new() -> Self {
        let schema: Value =
            serde_json::from_str(SCHEMA_JSON).expect("theorems.schema.json must be valid JSON");
        let registry = Self::build_registry();
        let tool_specs = registry.specs();
        Self { schema, tool_specs }
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
         id, type, complete statement with no ellipsis, raw source_tex when \
         available, and the section it lives in. For theorem/lemma/proposition/\
         corollary blocks, also transcribe the mathematical statement into \
         typed_transcription and theorem_ir. Use explicit unknown_type, \
         unknown_term, or unknown_prop nodes only for the exact subexpression \
         you cannot safely type; do not mark a complete theorem partial just \
         because the prose is long. When the conclusion is an (in)equality or a \
         logical combination, emit the precise kind (equals, less_equal, \
         less_than, greater_equal, greater_than, implies, and, or, not, forall, \
         exists) rather than falling back to unknown_prop. Proof blocks are \
         dependency evidence, not \
         Lean theorem targets, so do not invent theorem_ir for proof bodies. \
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
             definition/proof you find, record `id`, `type`, full `statement` \
             without `...`, `source_tex` when available, and `section`. For \
             theorem/lemma/proposition/corollary entries, fill \
             `typed_transcription` and `theorem_ir` from the LaTeX math. For \
             every `\\ref{{...}}` you see inside a proof, call `resolve_label` \
             and add the resolved id to that proof's `depends_on` list. Finally \
             call `submit` with the entire `theorem_graph`.",
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
        debug_assert!(
            ctx.max_cost_usd > 0.0,
            "ExtractionContext.max_cost_usd must be populated (FP-RPT3a A5)"
        );
        let max_iters = ctx.max_iters as usize;
        let max_cost_usd = ctx.max_cost_usd;
        crate::extraction::run_tool_loop(self, runner, spec, ctx, max_iters, max_cost_usd).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::r#loop::run_tool_loop;
    use crate::extraction::ExtractionContext;
    use crate::extraction::ToolCtx;
    use agenthero_agent_runtime::AgentRunner;
    use agenthero_agent_runtime::{AgentSpec, Message};
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{
        FinishReason, ProviderToolCall, ToolCompletion, ToolSpec as LlmToolSpec, Usage,
    };
    use grokrxiv_schemas::PaperExtract;
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
        AgentSpec::api_default("summary", "claude".to_string(), "claude-test".to_string())
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
            _input: &agenthero_agent_runtime::AgentInput,
        ) -> anyhow::Result<agenthero_agent_runtime::AgentRun> {
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
            ],
            "reason": null
        });
        assert!(validator.validate(&good).is_ok());
        // Missing depends_on -> invalid.
        let bad = json!({
            "theorem_graph": [
                {"id":"T1","type":"theorem","statement":"X.","section":"sec-2"}
            ],
            "reason": null
        });
        assert!(validator.validate(&bad).is_err());
    }

    #[test]
    fn schema_allows_typed_theorem_transcription_for_llm_math_ir() {
        let a = TheoremGraphExtractorAgent::new();
        let schema = a.schema();
        let validator = jsonschema::validator_for(schema).expect("schema compiles");
        let typed = json!({
            "theorem_graph": [
                {
                    "id": "thm-add-zero",
                    "type": "theorem",
                    "statement": "For every $n \\in \\mathbb{N}$, $n + 0 = n$.",
                    "section": "sec-main",
                    "depends_on": ["eq-add-zero"],
                    "source_tex": "\\begin{theorem}\\label{thm:add-zero} For every $n \\in \\mathbb{N}$, $n + 0 = n$.\\end{theorem}",
                    "typed_transcription": {
                        "status": "transcribed",
                        "source_text": "\\begin{theorem}\\label{thm:add-zero} For every $n \\in \\mathbb{N}$, $n + 0 = n$.\\end{theorem}",
                        "math_objects": [
                            {"name": "n", "type": {"kind": "nat"}}
                        ],
                        "binders": [
                            {"name": "n", "type": {"kind": "nat"}}
                        ],
                        "assumptions": [],
                        "conclusion": {
                            "kind": "equals",
                            "lhs": {
                                "kind": "add",
                                "lhs": {"kind": "var", "name": "n"},
                                "rhs": {"kind": "nat_lit", "value": 0}
                            },
                            "rhs": {"kind": "var", "name": "n"}
                        }
                    },
                    "theorem_ir": {
                        "theorem_name": "thm_add_zero",
                        "binders": [
                            {"name": "n", "type": {"kind": "nat"}}
                        ],
                        "assumptions": [],
                        "conclusion": {
                            "kind": "equals",
                            "lhs": {
                                "kind": "add",
                                "lhs": {"kind": "var", "name": "n"},
                                "rhs": {"kind": "nat_lit", "value": 0}
                            },
                            "rhs": {"kind": "var", "name": "n"}
                        }
                    }
                }
            ],
            "reason": null
        });

        assert!(
            validator.validate(&typed).is_ok(),
            "theorem extraction schema must carry typed math IR to review-loop"
        );
    }

    #[test]
    fn schema_allows_null_typed_ir_for_proofs_and_untranscribed_entries() {
        let a = TheoremGraphExtractorAgent::new();
        let schema = a.schema();
        let validator = jsonschema::validator_for(schema).expect("schema compiles");
        let proof = json!({
            "theorem_graph": [
                {
                    "id": "proof-main",
                    "type": "proof",
                    "statement": "Proof. The claim follows from Lemma 1.",
                    "section": "sec-proof",
                    "source_tex": null,
                    "typed_transcription": null,
                    "theorem_ir": null,
                    "depends_on": ["lem-1"]
                }
            ],
            "reason": null
        });

        assert!(
            validator.validate(&proof).is_ok(),
            "proof and nonformal entries may explicitly set typed fields to null"
        );
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
            max_cost_usd: 1.0,
            max_iters: 10,
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
            ],
            "reason": null
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
        let tool_sequence: Vec<&str> = run.tool_calls.iter().map(|c| c.tool.as_str()).collect();
        assert_eq!(
            tool_sequence,
            vec!["list_sections", "read_section", "resolve_label", "submit"]
        );
    }
}
