//! `MacroExpanderAgent` (Stage 4 — Track 8d).
//!
//! Tool-using extraction agent that normalizes LaTeX source by expanding every
//! user-defined macro (`\newcommand` / `\renewcommand` / `\providecommand` /
//! `\def` / `\DeclareMathOperator`) inline, so downstream extraction stages
//! (equation canonicalizer, theorem-graph extractor, citation contextualizer)
//! see plain TeX.
//!
//! The agent advertises five tools: the core `list_files`, `read_file`,
//! `submit`, plus this module's `find_definitions` and `apply_expansions`. The
//! LLM is responsible for the orchestration; we just supply deterministic
//! primitives.
//!
//! See [`tools`] for the regex/string parser and its balanced-brace
//! limitations.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agents::extraction::{ExtractionAgent, ExtractionRole, ToolRegistry};
use crate::agents::types::{ExtractionContext, ExtractionRun, ToolSpec};

pub mod tools;

pub use tools::{
    apply_expansions, extract_definitions, ApplyExpansionsTool, FindDefinitionsTool, MacroDef,
    MacroLookup,
};

/// Stage-4 macro expander. Carries the compiled output schema and the
/// advertised tool list.
pub struct MacroExpanderAgent {
    schema: Value,
    tool_specs: Vec<ToolSpec>,
}

impl Default for MacroExpanderAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroExpanderAgent {
    /// Build the agent with the embedded JSON schema and a tool registry that
    /// advertises exactly `[list_files, read_file, find_definitions, apply_expansions, submit]`.
    pub fn new() -> Self {
        let schema: Value = serde_json::from_str(SCHEMA_JSON)
            .expect("compiled-in macros.schema.json must parse");
        let mut registry = ToolRegistry::empty();
        registry.register(Arc::new(
            crate::agents::extraction::tools::list_files::ListFilesTool,
        ));
        registry.register(Arc::new(
            crate::agents::extraction::tools::read_file::ReadFileTool,
        ));
        registry.register(Arc::new(FindDefinitionsTool));
        registry.register(Arc::new(ApplyExpansionsTool));
        registry.register(Arc::new(
            crate::agents::extraction::tools::submit::SubmitTool,
        ));
        let tool_specs = registry.specs();
        Self { schema, tool_specs }
    }

    /// Build a [`ToolRegistry`] populated with exactly the tools this agent
    /// advertises. The orchestrator passes this in `ExtractionContext.registry`
    /// so the tool-call loop's `invoke_tool` can resolve names.
    pub fn registry(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::empty();
        registry.register(Arc::new(
            crate::agents::extraction::tools::list_files::ListFilesTool,
        ));
        registry.register(Arc::new(
            crate::agents::extraction::tools::read_file::ReadFileTool,
        ));
        registry.register(Arc::new(FindDefinitionsTool));
        registry.register(Arc::new(ApplyExpansionsTool));
        registry.register(Arc::new(
            crate::agents::extraction::tools::submit::SubmitTool,
        ));
        registry
    }
}

/// The schema is embedded at compile time so the agent is self-contained and
/// the unit tests don't have to know about the repo layout. The source of
/// truth is `schemas/extraction/macros.schema.json`.
const SCHEMA_JSON: &str = include_str!(
    "../../../../../../schemas/extraction/macros.schema.json"
);

#[async_trait]
impl ExtractionAgent for MacroExpanderAgent {
    fn name(&self) -> &'static str {
        "macro_expander"
    }
    fn role(&self) -> ExtractionRole {
        ExtractionRole::MacroExpander
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_specs.clone()
    }
    fn system_prompt(&self) -> String {
        "You are normalizing LaTeX source by expanding user-defined macros. \
         Use `list_files(glob:'**/*.tex')` to find TeX files, `read_file` to inspect them, \
         `find_definitions` to extract every `\\newcommand` / `\\def` / `\\renewcommand` / \
         `\\DeclareMathOperator`, then call `apply_expansions` to substitute occurrences. \
         Preserve all math semantics; do NOT rewrite equations beyond literal macro expansion. \
         Call `submit({normalized_tex, expansions_applied})` when done."
            .to_string()
    }
    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        format!(
            "Paper: {arxiv} (title: {title}). The unpacked TeX source bundle is at ./ (your \
             workdir). Find every user-defined macro definition across all *.tex files, build a \
             mapping, expand every occurrence in the concatenated source, and submit \
             {{normalized_tex, expansions_applied}}.",
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
        crate::agents::extraction::run_tool_loop(self, runner, spec, ctx, 20, 1.00).await
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::agents::extraction::ToolCtx;
    use crate::agents::traits::AgentRunner;
    use crate::agents::types::{AgentSpec, Message, ToolCompletion, ToolSpec};
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, Usage};
    use grokrxiv_schemas::{AgentRole, PaperExtract};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use uuid::Uuid;

    /// Local scripted runner — mirrors the (private) one in `loop::tests`.
    /// Returns each queued [`ToolCompletion`] in order, ignoring the messages
    /// / tools it receives.
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
            "scripted"
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
            _tools: &[ToolSpec],
            _ctx: &ToolCtx<'_>,
        ) -> anyhow::Result<ToolCompletion> {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("scripted runner queue exhausted");
            }
            Ok(q.remove(0))
        }
    }

    /// Local tempdir helper — same idea as the (private) one in `loop::tests`.
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
        p.push(format!("grokrxiv-macros-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn paper_extract() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2401.00001v1".into(),
            title: "A Toy Paper".into(),
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
            "claude-haiku-4-5".to_string(),
        )
    }

    fn turn_call(name: &str, args: Value, id: &str) -> ToolCompletion {
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

    #[test]
    fn macro_expander_advertises_correct_tools() {
        let agent = MacroExpanderAgent::new();
        let names: Vec<String> = agent.tools().iter().map(|t| t.name.clone()).collect();
        // `specs()` returns alphabetically sorted names — that's the order
        // we assert here so the test is deterministic.
        assert_eq!(
            names,
            vec![
                "apply_expansions".to_string(),
                "find_definitions".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
                "submit".to_string(),
            ],
            "MacroExpanderAgent must advertise exactly these five tools"
        );
        assert_eq!(agent.role(), ExtractionRole::MacroExpander);
        assert_eq!(agent.name(), "macro_expander");
    }

    #[tokio::test]
    async fn macro_expander_run_via_mock_runner() {
        // Set up a workdir with one tiny TeX file the agent's tools can read.
        let tmp = tempdir();
        let tex = r"\newcommand{\R}{\mathbb{R}}
Then \R^n is the space.";
        std::fs::write(tmp.path().join("main.tex"), tex).unwrap();

        // Sequence the mock runner: list_files -> read_file -> find_definitions
        // -> apply_expansions -> submit. The values returned by the
        // intermediate tools are ignored by the runner (the LLM would
        // normally read them); we only care that the loop drives through to
        // the submit payload.
        let final_payload = json!({
            "normalized_tex": "Then \\mathbb{R}^n is the space.",
            "expansions_applied": [
                { "name": "\\R", "body": "\\mathbb{R}", "occurrences": 1 }
            ]
        });
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_call("list_files", json!({"glob": "**/*.tex"}), "c1"),
            turn_call("read_file", json!({"path": "main.tex"}), "c2"),
            turn_call("find_definitions", json!({}), "c3"),
            turn_call(
                "apply_expansions",
                json!({
                    "input_text": tex,
                    "mapping": { "\\R": { "body": "\\mathbb{R}", "params": 0 } }
                }),
                "c4",
            ),
            turn_submit(final_payload.clone()),
        ]));

        let agent = MacroExpanderAgent::new();
        let registry = Arc::new(agent.registry());
        let pe = paper_extract();
        let ec = ExtractionContext {
            workdir: tmp.path(),
            extract: &pe,
            semantic_ast: None,
            paper_id: Uuid::nil(),
            arxiv_id: "2401.00001v1",
            registry,
        };
        let spec = fake_spec();
        let run = agent.run(runner, &spec, ec).await.expect("loop runs");
        assert_eq!(run.output, final_payload);
        assert!(
            run.tool_calls
                .iter()
                .any(|c| c.tool == "find_definitions" && c.ok),
            "find_definitions should appear in the audit log: {:?}",
            run.tool_calls
        );
        assert!(
            run.tool_calls
                .iter()
                .any(|c| c.tool == "apply_expansions" && c.ok),
            "apply_expansions should appear in the audit log: {:?}",
            run.tool_calls
        );
        let submit = run
            .tool_calls
            .iter()
            .find(|c| c.tool == "submit")
            .expect("submit recorded");
        assert!(submit.ok);
        assert_eq!(run.iters, 5);
    }
}
