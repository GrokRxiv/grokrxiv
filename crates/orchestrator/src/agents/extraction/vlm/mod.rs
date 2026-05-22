//! Stage 3 — [`VlmExtractorAgent`].
//!
//! Tool-using agent that walks a paper's PDF and emits the strict-JSON
//! extraction the rest of the pipeline consumes. Used when no TeX source is
//! available (PDF-only papers) or as a fidelity audit alongside the
//! deterministic Stage 2 path.
//!
//! The agent advertises four tools:
//! - `read_pdf_page(page)` — render a page + return its text layer
//! - `search_pdf(query)`   — locate a phrase across the PDF
//! - `extract_page_region(page, bbox)` — crop a figure/table region
//! - `submit(payload)`     — finalise with the strict-schema JSON payload
//!
//! Model: `gemini-2.5-pro` (native multimodal). Fallback: `claude-opus-4-7`.
//!
//! The agent itself doesn't know how to talk to the LLM — that's the
//! [`AgentRunner`]'s job. The tool-call loop in
//! [`crate::agents::extraction::r#loop::run_tool_loop`] does the rest.

pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{run_tool_loop, ExtractionAgent, ExtractionRole, ToolRegistry};
use crate::agents::types::{AgentSpec, ExtractionContext, ExtractionRun, ToolSpec};
use crate::agents::AgentRunner;

/// Built-in strict schema for the `submit(...)` payload. Mirrors
/// `schemas/extraction/vlm.schema.json` (OpenAI-strict-compatible: every
/// property in `required`, nullable fields use `["X","null"]`).
pub fn submit_schema() -> Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "abstract", "sections", "bibliography", "figures", "equations", "reason"],
        "properties": {
            "reason": {
                "description": "Optional escape hatch (FP-RPT3a A4): set to `paper_is_blank` when the PDF carries no extractable content.",
                "type": ["string", "null"],
                "enum": [null, "paper_is_blank"]
            },
            "title": { "type": "string" },
            "abstract": { "type": "string" },
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["heading", "body_markdown"],
                    "properties": {
                        "heading": { "type": "string" },
                        "body_markdown": { "type": "string" }
                    }
                }
            },
            "bibliography": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["raw", "doi", "arxiv_id"],
                    "properties": {
                        "raw": { "type": "string" },
                        "doi": { "type": ["string", "null"] },
                        "arxiv_id": { "type": ["string", "null"] }
                    }
                }
            },
            "figures": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["caption", "page"],
                    "properties": {
                        "caption": { "type": "string" },
                        "page": { "type": ["integer", "null"] }
                    }
                }
            },
            "equations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "tex", "context"],
                    "properties": {
                        "id": { "type": "string" },
                        "tex": { "type": "string" },
                        "context": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Build a [`ToolRegistry`] populated only with the four tools the VLM
/// extractor uses (`read_pdf_page`, `search_pdf`, `extract_page_region`,
/// `submit`). The shared-core tools are intentionally NOT included — the VLM
/// agent operates on a PDF, not an unpacked source bundle.
pub fn build_registry() -> ToolRegistry {
    let mut r = ToolRegistry::empty();
    r.register(Arc::new(tools::ReadPdfPageTool));
    r.register(Arc::new(tools::SearchPdfTool));
    r.register(Arc::new(tools::ExtractPageRegionTool));
    r.register(Arc::new(
        crate::agents::extraction::tools::submit::SubmitTool,
    ));
    r
}

/// Stage-3 PDF→structured-JSON extraction agent.
pub struct VlmExtractorAgent {
    schema: Value,
    tools: Vec<ToolSpec>,
    max_iters: usize,
    max_cost_usd: f32,
}

impl VlmExtractorAgent {
    /// Default loop budget for the VLM agent — generous enough to walk a
    /// long paper a page at a time. Mirrors `agents/extraction/vlm.yaml`.
    pub const DEFAULT_MAX_ITERS: usize = 40;
    /// Per-paper USD ceiling. Bumped from 0.10 in H2: when no TeX source is
    /// available the VLM is the only structural extractor and a generous
    /// budget is cheaper than failing to ingest the paper.
    pub const DEFAULT_MAX_COST_USD: f32 = 1.00;

    /// Construct a fresh agent with the default loop budget and a registry
    /// containing the four PDF tools.
    pub fn new() -> Self {
        let registry = build_registry();
        let tools = registry.specs();
        Self {
            schema: submit_schema(),
            tools,
            max_iters: Self::DEFAULT_MAX_ITERS,
            max_cost_usd: Self::DEFAULT_MAX_COST_USD,
        }
    }

    /// Override the per-paper iteration ceiling (for tests / per-run tuning).
    pub fn with_max_iters(mut self, n: usize) -> Self {
        self.max_iters = n;
        self
    }

    /// Override the per-paper USD ceiling.
    pub fn with_max_cost_usd(mut self, usd: f32) -> Self {
        self.max_cost_usd = usd;
        self
    }

    /// Execute the agent end-to-end through the tool-call loop. The
    /// `ExtractionContext` carries the workdir (where the pipeline already
    /// wrote `<arxiv_id>.pdf`) and the paper extract for metadata.
    pub async fn run_loop(
        &self,
        runner: Arc<dyn AgentRunner>,
        spec: &AgentSpec,
        ctx: ExtractionContext<'_>,
    ) -> anyhow::Result<ExtractionRun> {
        run_tool_loop(self, runner, spec, ctx, self.max_iters, self.max_cost_usd).await
    }
}

impl Default for VlmExtractorAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtractionAgent for VlmExtractorAgent {
    fn name(&self) -> &'static str {
        "vlm_extractor"
    }
    fn role(&self) -> ExtractionRole {
        ExtractionRole::VlmExtractor
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools.clone()
    }
    fn system_prompt(&self) -> String {
        "You are extracting structured content from a research paper's PDF. The user kickoff \
         message will give you the PDF location. Use `read_pdf_page` to inspect specific pages, \
         `search_pdf` to locate phrases, `extract_page_region` to pull figures/tables. Call \
         `submit(...)` with the complete extraction when done. Output a single JSON object \
         matching the supplied schema; no commentary, no fences."
            .to_string()
    }
    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        let schema_str =
            serde_json::to_string_pretty(&self.schema).unwrap_or_else(|_| "{}".to_string());
        let tool_names: Vec<String> = self.tools.iter().map(|t| t.name.clone()).collect();
        format!(
            "Paper: {arxiv}\n\
             PDF location: ./{arxiv}.pdf (relative to your workdir)\n\
             \n\
             You have these tools available: {tools}.\n\
             \n\
             Produce a single `submit(...)` call whose JSON argument validates against this \
             schema:\n\
             ```json\n{schema}\n```\n\
             \n\
             Begin by calling `read_pdf_page(page=1)` to see the title page; iterate through the \
             paper as you go. Stop when you have a complete extraction — no commentary, just the \
             final `submit`.",
            arxiv = ctx.arxiv_id,
            tools = tool_names.join(", "),
            schema = schema_str,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::extraction::ToolRegistry;
    use crate::agents::types::{AgentSpec, ExtractionContext, Message, ToolCallRecord};
    use crate::agents::AgentRunner;
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, ToolCompletion, ToolSpec, Usage};
    use grokrxiv_schemas::PaperExtract;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Minimal scripted runner mirroring the one in the loop tests — returns
    /// pre-canned [`ToolCompletion`]s in order.
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
            "scripted-vlm"
        }
        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &crate::agents::types::AgentInput,
        ) -> anyhow::Result<crate::agents::types::AgentRun> {
            unimplemented!("vlm scripted runner is tool-only")
        }
        async fn complete_with_tools(
            &self,
            _spec: &AgentSpec,
            _messages: &[Message],
            tools: &[ToolSpec],
            _ctx: &crate::agents::extraction::ToolCtx<'_>,
        ) -> anyhow::Result<ToolCompletion> {
            self.seen_tools
                .lock()
                .unwrap()
                .push(tools.iter().map(|t| t.name.clone()).collect());
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("scripted-vlm queue exhausted");
            }
            Ok(q.remove(0))
        }
    }

    fn turn_tool(name: &str, args: Value, id: &str) -> ToolCompletion {
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
    fn turn_submit(payload: Value) -> ToolCompletion {
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

    fn valid_payload() -> Value {
        json!({
            "title": "A Toy Paper on Category Theory",
            "abstract": "Short abstract.",
            "sections": [
                { "heading": "Introduction", "body_markdown": "Hello." }
            ],
            "bibliography": [
                { "raw": "Smith, 2024", "doi": null, "arxiv_id": null }
            ],
            "figures": [
                { "caption": "Fig 1", "page": 1 }
            ],
            "equations": [
                { "id": "eq1", "tex": "x^2", "context": "Intro" }
            ],
            "reason": null
        })
    }

    fn paper_extract() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2401.00001v1".into(),
            title: "Toy".into(),
            authors: vec![],
            abstract_: "abs".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    fn spec() -> AgentSpec {
        AgentSpec::api_default(
            "summary",
            "gemini".to_string(),
            "gemini-2.5-pro".to_string(),
        )
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
            "grokrxiv-vlm-agent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn write_fixture_pdf(workdir: &std::path::Path, arxiv_id: &str) {
        let pdf = super::tools::build_fixture_pdf();
        std::fs::write(workdir.join(format!("{arxiv_id}.pdf")), pdf).unwrap();
    }

    fn ctx<'a>(
        workdir: &'a std::path::Path,
        extract: &'a PaperExtract,
        registry: Arc<ToolRegistry>,
        arxiv_id: &'a str,
    ) -> ExtractionContext<'a> {
        ExtractionContext {
            workdir,
            extract,
            semantic_ast: None,
            paper_id: uuid::Uuid::nil(),
            arxiv_id,
            registry,
            max_cost_usd: 1.0,
            max_iters: 5,
        }
    }

    #[test]
    fn vlm_agent_advertises_correct_tools() {
        let agent = VlmExtractorAgent::new();
        let names: Vec<String> = agent.tools().iter().map(|t| t.name.clone()).collect();
        for required in [
            "read_pdf_page",
            "search_pdf",
            "extract_page_region",
            "submit",
        ] {
            assert!(
                names.contains(&required.to_string()),
                "VLM agent must advertise `{required}`; got {names:?}"
            );
        }
        assert_eq!(agent.role(), ExtractionRole::VlmExtractor);
        assert_eq!(agent.name(), "vlm_extractor");
    }

    #[test]
    fn vlm_agent_rejects_invalid_submit_schema() {
        // Sanity-check: the agent's schema rejects a payload missing `title`.
        // We invoke jsonschema directly — this proves `.schema()` is correctly
        // populated so the loop's validate_submit() will reject bad payloads.
        let agent = VlmExtractorAgent::new();
        let bad = json!({
            "abstract": "no title field!",
            "sections": [],
            "bibliography": [],
            "figures": [],
            "equations": []
        });
        let validator = jsonschema::validator_for(agent.schema()).expect("schema compiles");
        let errors: Vec<String> = validator.iter_errors(&bad).map(|e| e.to_string()).collect();
        assert!(
            errors.iter().any(|e| e.contains("title")),
            "expected a title-related schema error, got {errors:?}"
        );
    }

    #[tokio::test]
    async fn vlm_agent_run_via_mock_runner() {
        let agent = VlmExtractorAgent::new()
            .with_max_iters(5)
            .with_max_cost_usd(1.0);
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_tool("read_pdf_page", json!({ "page": 1 }), "call_read"),
            turn_submit(valid_payload()),
        ]));
        let tmp = tempdir();
        write_fixture_pdf(tmp.path(), "2401.00001v1");
        let pe = paper_extract();
        let registry = Arc::new(build_registry());
        let ec = ctx(tmp.path(), &pe, registry, "2401.00001v1");
        let s = spec();

        let run = agent.run_loop(runner, &s, ec).await.expect("loop succeeds");
        assert_eq!(run.iters, 2, "two model turns: one tool + one submit");
        let tool_calls = &run.tool_calls;
        assert!(
            tool_calls.iter().any(|c| c.tool == "read_pdf_page" && c.ok),
            "expected a successful read_pdf_page call in audit log, got {tool_calls:?}"
        );
        assert!(
            tool_calls.iter().any(|c| c.tool == "submit" && c.ok),
            "expected a successful submit in audit log"
        );
        assert_eq!(
            run.output
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "A Toy Paper on Category Theory"
        );
    }

    #[tokio::test]
    async fn vlm_agent_validates_submit_payload_through_loop() {
        // The loop should reject the first (invalid) submit and accept the
        // corrective second submit. This confirms `.schema()` is wired into
        // the loop's validate_submit hook, not just locally on the agent.
        let agent = VlmExtractorAgent::new()
            .with_max_iters(5)
            .with_max_cost_usd(1.0);
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_submit(json!({ "abstract": "missing title" })),
            turn_submit(valid_payload()),
        ]));
        let tmp = tempdir();
        write_fixture_pdf(tmp.path(), "2401.00002v1");
        let pe = paper_extract();
        let registry = Arc::new(build_registry());
        let ec = ctx(tmp.path(), &pe, registry, "2401.00002v1");
        let s = spec();

        let run = agent
            .run_loop(runner, &s, ec)
            .await
            .expect("retry succeeds");
        let submits: Vec<&ToolCallRecord> = run
            .tool_calls
            .iter()
            .filter(|c| c.tool == "submit")
            .collect();
        assert_eq!(submits.len(), 2);
        assert!(!submits[0].ok, "first submit should be marked failed");
        assert!(submits[1].ok, "second submit should be marked success");
    }
}
