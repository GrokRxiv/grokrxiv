//! Stage 7 — `CitationContextualizerAgent`.
//!
//! Tool-using agent that walks `body.md` for every `[@key]` citation use,
//! resolves real bibliographic metadata via CrossRef / arXiv (NEVER inventing
//! DOIs), and classifies each use site semantically. Emits an enriched
//! citation graph that downstream review agents and the GrokRxiv corpus
//! index can consume.
//!
//! Loop budget — `max_iters=80` (one+ tool call per citation site is the
//! expected pattern), `max_cost_usd=0.05`. Model: `gemini-2.5-pro` (strong
//! at structured classification + native tool use).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{
    tools as core_tools, ExtractionAgent, ExtractionContext, ExtractionRole, ExtractionRun, Tool,
    ToolSpec,
};
use crate::agents::traits::AgentRunner;
use crate::agents::types::AgentSpec;

pub mod tools;

/// Per-agent registered tools (excluding `submit`, which the loop intercepts).
fn citation_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(tools::ListCitationSitesTool),
        Arc::new(tools::LookupBibtexTool),
        Arc::new(tools::SearchCorpusTool),
        Arc::new(tools::ReadSectionTool),
        Arc::new(core_tools::crossref_lookup::CrossrefLookupTool),
        Arc::new(core_tools::arxiv_lookup::ArxivLookupTool),
    ]
}

/// Schema the final `submit(...)` payload must satisfy. Mirrors
/// `schemas/extraction/citations.schema.json`.
fn output_schema() -> Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "additionalProperties": false,
        "required": ["citations"],
        "properties": {
            "citations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["key", "raw", "resolved_doi", "resolved_arxiv_id", "contexts"],
                    "properties": {
                        "key": { "type": "string" },
                        "raw": { "type": "string" },
                        "resolved_doi": { "type": ["string", "null"] },
                        "resolved_arxiv_id": { "type": ["string", "null"] },
                        "contexts": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["section", "sentence", "use"],
                                "properties": {
                                    "section": { "type": "string" },
                                    "sentence": { "type": "string" },
                                    "use": {
                                        "type": "string",
                                        "enum": [
                                            "extends",
                                            "contradicts",
                                            "relies_on",
                                            "compared_with",
                                            "cited_in_passing",
                                            "background"
                                        ]
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Stage 7 agent implementation. Construct with [`CitationContextualizerAgent::new`].
pub struct CitationContextualizerAgent {
    schema: Value,
    tool_specs: Vec<ToolSpec>,
}

impl CitationContextualizerAgent {
    /// Build the agent with its full tool set advertised.
    pub fn new() -> Self {
        // Advertised specs: per-agent tools + the two relevant core tools +
        // the `submit` sentinel. The submit spec's schema is the agent's
        // output schema (the loop validates against the same value).
        let mut tool_specs: Vec<ToolSpec> = citation_tools()
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.schema().clone(),
            })
            .collect();
        let schema = output_schema();
        tool_specs.push(ToolSpec {
            name: "submit".to_string(),
            description:
                "Finalise extraction with the enriched citation graph. Payload MUST match the agent schema."
                    .to_string(),
            input_schema: schema.clone(),
        });
        Self { schema, tool_specs }
    }
}

impl Default for CitationContextualizerAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtractionAgent for CitationContextualizerAgent {
    fn name(&self) -> &'static str {
        "citation_contextualizer"
    }
    fn role(&self) -> ExtractionRole {
        ExtractionRole::CitationContextualizer
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_specs.clone()
    }
    fn system_prompt(&self) -> String {
        "You are enriching this paper's citation graph. Use `list_citation_sites` \
         to find every [@key] occurrence, `lookup_bibtex(key)` to get the raw \
         entry, `crossref_lookup` or `arxiv_lookup` to fetch real metadata \
         (DOIs, arxiv IDs — DO NOT INVENT THEM), and `search_corpus(query)` to \
         find related already-reviewed papers in the GrokRxiv corpus. For each \
         citation, classify each use site as one of: extends, contradicts, \
         relies_on, compared_with, cited_in_passing, background. Submit the \
         enriched citation graph by calling submit(...) with a payload matching \
         the schema. If a metadata lookup returns no result, set `resolved_doi` \
         or `resolved_arxiv_id` to null — never fabricate identifiers."
            .to_string()
    }
    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        format!(
            "Paper {arxiv} (title: {title}). The unpacked source is at ./ \
             (your workdir); the normalized markdown is `body.md` and the \
             bibliography lives in `*.bib`. Start by calling list_citation_sites \
             — it will return every [@key] use with its containing section and \
             sentence. Then for each unique key, fetch metadata and classify \
             every site. Submit one entry per unique citation key.",
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
        crate::agents::extraction::run_tool_loop(self, runner, spec, ctx, 80, 0.50).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::extraction::ToolRegistry;
    use crate::agents::types::{AgentInput, AgentRun, AgentSpec, ExtractionContext, Message};
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, ToolCompletion, Usage};
    use grokrxiv_schemas::{AgentRole, PaperExtract};
    use serde_json::json;
    use std::sync::Mutex;

    /// Local scripted runner; the loop's `ScriptedRunner` lives behind a
    /// `cfg(test)` module that's private to its own crate-test scope, so we
    /// re-declare a thin equivalent here.
    struct ScriptedRunner {
        queue: Mutex<Vec<ToolCompletion>>,
    }
    impl ScriptedRunner {
        fn new(turns: Vec<ToolCompletion>) -> Self {
            Self {
                queue: Mutex::new(turns),
            }
        }
    }
    #[async_trait]
    impl AgentRunner for ScriptedRunner {
        fn name(&self) -> &'static str {
            "scripted-d5"
        }
        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &AgentInput,
        ) -> anyhow::Result<AgentRun> {
            unimplemented!("scripted runner is tool-only")
        }
        async fn complete_with_tools(
            &self,
            _spec: &AgentSpec,
            _messages: &[Message],
            _tools: &[ToolSpec],
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
            title: "Citation Test Paper".into(),
            authors: vec![],
            abstract_: "abs".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
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
                id: "submit".into(),
                name: "submit".into(),
                arguments: payload,
            }],
            text: String::new(),
            finish_reason: FinishReason::ToolUse,
            usage: Usage::default(),
            raw: json!({}),
        }
    }

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grokrxiv-d5-agent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    #[test]
    fn citation_context_advertises_correct_tools() {
        let agent = CitationContextualizerAgent::new();
        let specs = agent.tools();
        let names: Vec<&str> = specs.iter().map(|t| t.name.as_str()).collect();
        for required in [
            "list_citation_sites",
            "lookup_bibtex",
            "search_corpus",
            "read_section",
            "crossref_lookup",
            "arxiv_lookup",
            "submit",
        ] {
            assert!(
                names.contains(&required),
                "expected {required} in advertised tools, got {names:?}"
            );
        }
    }

    /// A registry that knows about the agent-scoped tools so the loop can
    /// dispatch them. We register both core tools (which the agent uses for
    /// `crossref_lookup`) and the citation-specific ones.
    fn registry_with_citation_tools() -> Arc<ToolRegistry> {
        let mut r = ToolRegistry::empty();
        r.register(Arc::new(tools::ListCitationSitesTool));
        r.register(Arc::new(tools::LookupBibtexTool));
        r.register(Arc::new(tools::SearchCorpusTool));
        r.register(Arc::new(tools::ReadSectionTool));
        r.register(Arc::new(
            crate::agents::extraction::tools::crossref_lookup::CrossrefLookupTool,
        ));
        r.register(Arc::new(
            crate::agents::extraction::tools::arxiv_lookup::ArxivLookupTool,
        ));
        r.register(Arc::new(
            crate::agents::extraction::tools::submit::SubmitTool,
        ));
        Arc::new(r)
    }

    #[tokio::test]
    async fn citation_context_run_via_mock_runner() {
        let dir = tempdir();
        // body.md with one citation site so list_citation_sites has work.
        let body = "## Intro\n\nWe build on [@foo2024].\n";
        std::fs::write(dir.0.join("body.md"), body).unwrap();
        // refs.bib with the matching key.
        std::fs::write(
            dir.0.join("refs.bib"),
            "@article{foo2024, title={Test}, author={Alice}, year={2024}, doi={10.1/x}}",
        )
        .unwrap();

        // Scripted runner: list_citation_sites -> lookup_bibtex -> crossref_lookup
        // (no env var so it'll hit api.crossref.org over real network; that's
        // outside the test's contract. To stay hermetic we instead route
        // crossref to a localhost URL that will fail and produce a `found:false`
        // result — which is fine, the loop carries on.) -> read_section -> submit.
        // We instead skip crossref to keep the test offline: list_citation_sites
        // -> lookup_bibtex -> read_section -> submit. That still proves the
        // tool-call sequence + ExtractionRun shape.
        std::env::set_var("GROKRXIV_CROSSREF_BASE", "http://127.0.0.1:1/_grokrxiv_block");
        let payload = json!({
            "citations": [{
                "key": "foo2024",
                "raw": "@article{foo2024, ...}",
                "resolved_doi": "10.1/x",
                "resolved_arxiv_id": null,
                "contexts": [{
                    "section": "Intro",
                    "sentence": "We build on [@foo2024].",
                    "use": "extends"
                }]
            }]
        });
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_call("list_citation_sites", json!({}), "c1"),
            turn_call("lookup_bibtex", json!({"key": "foo2024"}), "c2"),
            turn_call("crossref_lookup", json!({"doi": "10.1/x"}), "c3"),
            turn_call("read_section", json!({"id": "sec-1"}), "c4"),
            turn_submit(payload.clone()),
        ]));

        let agent = CitationContextualizerAgent::new();
        let pe = paper_extract();
        let registry = registry_with_citation_tools();
        let ctx = ExtractionContext {
            workdir: dir.0.as_path(),
            extract: &pe,
            semantic_ast: None,
            paper_id: uuid::Uuid::nil(),
            arxiv_id: "2401.99999v1",
            registry,
        };
        let spec = AgentSpec::api_default(
            AgentRole::Citation,
            "gemini".to_string(),
            "gemini-2.5-pro".to_string(),
        );
        let run = agent.run(runner, &spec, ctx).await.expect("loop ok");
        assert_eq!(run.output, payload);
        // Verify the tool-call log has the expected sequence (submit is last).
        let tools_called: Vec<&str> = run
            .tool_calls
            .iter()
            .map(|c| c.tool.as_str())
            .collect();
        assert_eq!(
            tools_called,
            vec![
                "list_citation_sites",
                "lookup_bibtex",
                "crossref_lookup",
                "read_section",
                "submit"
            ]
        );
        // list_citation_sites and lookup_bibtex must have succeeded; the
        // unreachable crossref will be ok=false with a `_error` but the loop
        // accepts that.
        assert!(run.tool_calls[0].ok, "list_citation_sites: {:?}", run.tool_calls[0]);
        assert!(run.tool_calls[1].ok, "lookup_bibtex: {:?}", run.tool_calls[1]);
        assert!(run.tool_calls[3].ok, "read_section: {:?}", run.tool_calls[3]);
        assert!(run.tool_calls[4].ok, "submit ok");
        std::env::remove_var("GROKRXIV_CROSSREF_BASE");
    }
}
