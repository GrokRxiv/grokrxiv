//! Tool-using extraction-agent framework.
//!
//! Stage-3+ extraction agents are NOT single-prompt-then-parse-JSON workers.
//! They run a **multi-turn tool-call loop**: the LLM iteratively invokes tools
//! (`read_file`, `query_ast`, `crossref_lookup`, ...) and finishes by calling
//! a sentinel `submit(...)` tool whose payload is validated against the
//! agent's schema. This is the same pattern as Anthropic tool_use, OpenAI
//! function calling, and Gemini function calling.
//!
//! This module defines the framework only — the [`ExtractionAgent`] trait,
//! the [`Tool`] trait, the shared [`ToolRegistry`], a core toolkit, and
//! [`run_tool_loop`] that drives the conversation. The five concrete
//! extraction agents (VLM extractor, macro expander, equation canonicaliser,
//! theorem-graph extractor, citation contextualiser) are wired in Wave 2.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use crate::agents::types::{
    ExtractionContext, ExtractionRun, Message, ToolCall, ToolCallRecord, ToolCompletion, ToolSpec,
};

pub mod r#loop;
pub mod tools;
pub mod vlm;

pub use r#loop::run_tool_loop;

/// Which extraction role this agent fills. There are exactly five — they
/// correspond 1:1 to Stages 3..7 in the ingest pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionRole {
    /// Stage 3: extract a paper extract from PDF bytes via a vision LLM.
    VlmExtractor,
    /// Stage 4: expand `\newcommand` / `\def` / `\DeclareMathOperator` chains.
    MacroExpander,
    /// Stage 5: canonicalise + dedup equations into MathML + semantic tags.
    EquationCanonicalizer,
    /// Stage 6: build the theorem dependency graph from cross-references.
    TheoremGraphExtractor,
    /// Stage 7: enrich citations with resolved metadata + use-context.
    CitationContextualizer,
}

impl ExtractionRole {
    /// Stable snake-case identifier used in logs and the `ExtractionRun.tool_calls` audit.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VlmExtractor => "vlm_extractor",
            Self::MacroExpander => "macro_expander",
            Self::EquationCanonicalizer => "equation_canonicalizer",
            Self::TheoremGraphExtractor => "theorem_graph_extractor",
            Self::CitationContextualizer => "citation_contextualizer",
        }
    }
}

/// Borrowed context handed to every [`Tool::call`]. Tools should treat this as
/// read-only; mutation lives inside the orchestrator.
pub struct ToolCtx<'a> {
    /// Working directory the bundle was unpacked into. `list_files` /
    /// `read_file` are constrained to this subtree.
    pub workdir: &'a Path,
    /// Optional semantic AST for `query_ast`.
    pub semantic_ast: Option<&'a serde_json::Value>,
    /// arXiv identifier — `arxiv_lookup` uses this when its `arxiv_id`
    /// argument is omitted.
    pub arxiv_id: &'a str,
    /// Shared `reqwest` client for tools that talk to upstream APIs.
    pub http: Arc<reqwest::Client>,
}

impl<'a> ToolCtx<'a> {
    /// Build a minimal [`ToolCtx`] from an [`ExtractionContext`]. The HTTP
    /// client is lazily constructed; callers can override with
    /// [`Self::with_http`] for tests.
    pub fn from_extraction(ctx: &'a ExtractionContext<'a>) -> Self {
        Self {
            workdir: ctx.workdir,
            semantic_ast: ctx.semantic_ast,
            arxiv_id: ctx.arxiv_id,
            http: Arc::new(reqwest::Client::new()),
        }
    }

    /// Override the HTTP client (used by `wiremock` tests).
    pub fn with_http(mut self, http: Arc<reqwest::Client>) -> Self {
        self.http = http;
        self
    }
}

/// One tool the LLM may invoke. Implementations live under [`tools`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (stable identifier the LLM will reference).
    fn name(&self) -> &'static str;
    /// One-line description shown to the LLM.
    fn description(&self) -> &'static str;
    /// JSON Schema for the tool's input arguments.
    fn schema(&self) -> &serde_json::Value;
    /// Execute the tool with arguments parsed from the LLM's call.
    async fn call(
        &self,
        args: serde_json::Value,
        ctx: &ToolCtx<'_>,
    ) -> anyhow::Result<serde_json::Value>;
}

/// Registry of tools available to an extraction agent. Pre-registered with the
/// shared core toolkit; per-agent extras can be added via [`Self::register`].
pub struct ToolRegistry {
    by_name: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Build an empty registry. Most callers want [`Self::with_core_tools`].
    pub fn empty() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    /// Build a registry pre-populated with the shared core toolkit.
    pub fn new() -> Self {
        Self::with_core_tools()
    }

    /// Build a registry pre-populated with the shared core toolkit:
    /// `list_files`, `read_file`, `query_ast`, `crossref_lookup`,
    /// `arxiv_lookup`, and the `submit` sentinel.
    pub fn with_core_tools() -> Self {
        let mut r = Self::empty();
        r.register(Arc::new(tools::list_files::ListFilesTool));
        r.register(Arc::new(tools::read_file::ReadFileTool));
        r.register(Arc::new(tools::query_ast::QueryAstTool));
        r.register(Arc::new(tools::crossref_lookup::CrossrefLookupTool));
        r.register(Arc::new(tools::arxiv_lookup::ArxivLookupTool));
        r.register(Arc::new(tools::submit::SubmitTool));
        r
    }

    /// Register (or replace) a tool. Per-agent extras go through here.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.by_name.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.by_name.get(name).cloned()
    }

    /// All tool specs in name-sorted order (stable for tests).
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut names: Vec<&String> = self.by_name.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let tool = self.by_name.get(n).expect("present");
                ToolSpec {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.schema().clone(),
                }
            })
            .collect()
    }
}

/// One Stage-3+ extraction agent. Implementations are NOT in this crate at
/// Wave 1 — they ship in Wave 2 under `agents/extraction/<role>.rs`. The
/// framework's only contract is the trait below and [`run_tool_loop`].
#[async_trait]
pub trait ExtractionAgent: Send + Sync {
    /// Stable agent name (typically the role's snake-case slug).
    fn name(&self) -> &'static str;
    /// Which Stage-3+ role this agent fills.
    fn role(&self) -> ExtractionRole;
    /// Output JSON Schema the final `submit(...)` payload must satisfy.
    fn schema(&self) -> &serde_json::Value;
    /// Tools advertised to the LLM this turn. Concrete agents typically clone
    /// the registry's specs and append agent-specific extras.
    fn tools(&self) -> Vec<ToolSpec>;
    /// System prompt for the agent. Default: a small generic preamble; concrete
    /// agents override.
    fn system_prompt(&self) -> String {
        format!(
            "You are the {} extraction agent for grokrxiv. Use the supplied tools \
             to inspect the paper, then finish by calling submit(...) with a JSON \
             payload matching the schema you were given. Do NOT emit prose; the \
             ONLY way to finish is by calling submit.",
            self.role().as_str()
        )
    }
    /// User-turn kickoff message. Default: a short paper-summary blurb;
    /// concrete agents override with role-specific context.
    fn user_kickoff(&self, ctx: &ExtractionContext<'_>) -> String {
        format!(
            "Paper: {arxiv} (title: {title}). The unpacked source bundle is at \
             ./ (your workdir). Use the tools to extract what your role needs, \
             then call submit(...).",
            arxiv = ctx.arxiv_id,
            title = ctx.extract.title,
        )
    }
    /// Run the agent end-to-end. Default delegates to [`run_tool_loop`] with
    /// the agent's own configuration; concrete agents can override for
    /// custom behaviour.
    async fn run(
        &self,
        runner: Arc<dyn crate::agents::traits::AgentRunner>,
        spec: &crate::agents::types::AgentSpec,
        ctx: ExtractionContext<'_>,
    ) -> anyhow::Result<ExtractionRun>
    where
        Self: Sized,
    {
        run_tool_loop(self, runner, spec, ctx, 30, 5.0).await
    }
}
