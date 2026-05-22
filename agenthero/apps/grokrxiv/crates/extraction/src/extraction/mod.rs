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
use grokrxiv_ingest::PaperExtract;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use agenthero_agent_runtime::{
    AgentRunner, AgentSpec, Message, ToolCall, ToolCompletion, ToolCtx, ToolSpec,
};

pub mod citations;
pub mod equations;
pub mod r#loop;
pub mod macros;
pub mod theorems;
pub mod tools;
pub mod vlm;

pub use r#loop::run_tool_loop;

/// Context handed to GrokRxiv extraction agents.
pub struct ExtractionContext<'a> {
    /// Working directory rooted at the unpacked paper source bundle.
    pub workdir: &'a Path,
    /// The paper extract (sections, bibliography, figures, ...).
    pub extract: &'a PaperExtract,
    /// Optional LaTeXML-derived semantic AST.
    pub semantic_ast: Option<&'a serde_json::Value>,
    /// DB UUID of the paper this extraction is running against.
    pub paper_id: Uuid,
    /// arXiv identifier (version-suffixed).
    pub arxiv_id: &'a str,
    /// Toolkit available this run.
    pub registry: Arc<ToolRegistry>,
    /// Per-stage dollar ceiling.
    pub max_cost_usd: f32,
    /// Per-stage iteration ceiling.
    pub max_iters: u32,
}

/// One audit-log record per tool call inside a tool-call loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Iteration number (zero-based) the call occurred on.
    pub iter: u32,
    /// Tool name.
    pub tool: String,
    /// Arguments the model passed to the tool.
    pub arguments: serde_json::Value,
    /// Tool result that came back.
    pub result: serde_json::Value,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Wall-clock duration of the call in milliseconds.
    pub latency_ms: i64,
}

/// Result of running a GrokRxiv extraction agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionRun {
    /// Final validated `submit(...)` payload.
    pub output: serde_json::Value,
    /// Audit log of every tool call.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Rough USD cost.
    pub cost_usd: f32,
    /// Wall-clock latency end-to-end in milliseconds.
    pub latency_ms: i64,
    /// Number of model turns consumed.
    pub iters: u32,
}

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

/// Build a minimal generic [`ToolCtx`] from a GrokRxiv extraction context.
pub fn tool_ctx_from_extraction<'a>(ctx: &'a ExtractionContext<'a>) -> ToolCtx<'a> {
    ToolCtx {
        workdir: ctx.workdir,
        semantic_ast: ctx.semantic_ast,
        source_id: ctx.arxiv_id,
        http: Arc::new(reqwest::Client::new()),
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
        r.register(Arc::new(tools::openalex_lookup::OpenAlexLookupTool));
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
        runner: Arc<dyn agenthero_agent_runtime::AgentRunner>,
        spec: &agenthero_agent_runtime::AgentSpec,
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
        run_tool_loop(self, runner, spec, ctx, max_iters, max_cost_usd).await
    }
}
