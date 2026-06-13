//! RPT3 Wave-3 — staged ingest pipeline orchestrator.
//!
//! Wires Stages 1–8 of the 8-stage extraction pipeline into a single
//! [`run_ingest_pipeline`] entry point:
//!
//! 1. **Stage 1 (ingest source)** — arXiv metadata + PDF + tar.gz acquisition.
//! 2. **Stage 2 (tool)** — TeX → reviewable Markdown or PDF → text, with
//!    optional LaTeXML semantic AST enrichment.
//! 3. **Stage 3 (agent)** — `VlmExtractorAgent`, only when no TeX source exists.
//! 4. **Stages 4–7 (tools by default, agents when enabled)** — macros,
//!    equations, theorems, citations. The default `pandoc_enabled` mode uses
//!    Rust/Pandoc-derived scanners; `agent_enabled` runs LLM tool loops and
//!    falls back to the local scanners when a stage returns no useful output.
//! 5. **Stage 8 (artifact)** — `PaperArtifacts::persist` writes Tier 1
//!    (Git) + Tier 2 (Supabase), then `db::persist_paper_extraction` updates
//!    Tier 3 (Postgres pointers + status).
//!
//! Idempotency: at boot we read `paper_assets.extraction_status`. `ready` →
//! short-circuit (unless `--no-cache`); `running` → caller decides whether to
//! wait or bail; `pending`/`failed` → run the pipeline.
//!
//! Graceful-degradation contract:
//! - Stage 2 failure on a PDF-only paper is expected; Stage 3 (VLM) takes
//!   over.
//! - In `pandoc_enabled` mode, Stages 4–7 are Rust function/tool calls. In
//!   `agent_enabled` mode, each LLM extraction agent is wrapped in
//!   [`run_agent_safe`]; failures record `degraded` in `extraction_report.json`
//!   and the local scanners backstop equations/theorems/citations.
//! - Stage 8 failure is fatal: we flip `extraction_status = 'failed'` so the
//!   next run picks it back up.

#![cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use grokrxiv_ingest::{
    DeterministicIngest, PaperExtract, TexBodyProducer as IngestTexBodyProducer,
};
use grokrxiv_storage::{
    ArtifactBundle, GitArtifactStore, PaperArtifacts, PersistedPointer, ReviewInput,
    SupabaseStorage,
};

use crate::agents::types::AgentRunnerKind;
use crate::agents::types::AgentSpec;
use crate::agents::AgentRunner;
use crate::db;
use crate::runtime_config::{parse_extractor, ExtractorKind};
use crate::state::AppState;
use grokrxiv_extraction::extraction::{
    citations::CitationContextualizerAgent, equations::EquationCanonicalizerAgent,
    macros::MacroExpanderAgent, theorems::TheoremGraphExtractorAgent, vlm::VlmExtractorAgent,
    ExtractionAgent, ExtractionContext, ToolCallRecord, ToolRegistry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtractionMode {
    PandocEnabled,
    AgentEnabled,
}

impl ExtractionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::PandocEnabled => "pandoc_enabled",
            Self::AgentEnabled => "agent_enabled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutorKind {
    IngestSource,
    Tool,
    Agent,
    Artifact,
}

impl ExecutorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::IngestSource => "ingest_source",
            Self::Tool => "tool",
            Self::Agent => "agent",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeProvenance {
    node: String,
    executor: ExecutorKind,
    tool: Option<String>,
    agent: Option<String>,
    mode: ExtractionMode,
    inputs: Vec<String>,
    outputs: Vec<String>,
}

impl NodeProvenance {
    fn ingest_source(
        node: impl Into<String>,
        tool: impl Into<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self::new(
            node,
            ExecutorKind::IngestSource,
            Some(tool.into()),
            None,
            inputs,
            outputs,
            mode,
        )
    }

    fn rust_tool(
        node: impl Into<String>,
        tool: impl Into<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self::new(
            node,
            ExecutorKind::Tool,
            Some(tool.into()),
            None,
            inputs,
            outputs,
            mode,
        )
    }

    fn cli_tool(
        node: impl Into<String>,
        tool: impl Into<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self::new(
            node,
            ExecutorKind::Tool,
            Some(tool.into()),
            None,
            inputs,
            outputs,
            mode,
        )
    }

    fn agent(
        node: impl Into<String>,
        agent: impl Into<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self::new(
            node,
            ExecutorKind::Agent,
            None,
            Some(agent.into()),
            inputs,
            outputs,
            mode,
        )
    }

    fn artifact(
        node: impl Into<String>,
        tool: impl Into<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self::new(
            node,
            ExecutorKind::Artifact,
            Some(tool.into()),
            None,
            inputs,
            outputs,
            mode,
        )
    }

    fn new(
        node: impl Into<String>,
        executor: ExecutorKind,
        tool: Option<String>,
        agent: Option<String>,
        inputs: impl IntoIterator<Item = impl Into<String>>,
        outputs: impl IntoIterator<Item = impl Into<String>>,
        mode: ExtractionMode,
    ) -> Self {
        Self {
            node: node.into(),
            executor,
            tool,
            agent,
            mode,
            inputs: inputs.into_iter().map(Into::into).collect(),
            outputs: outputs.into_iter().map(Into::into).collect(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "node": self.node,
            "executor": self.executor.as_str(),
            "tool": self.tool,
            "agent": self.agent,
            "mode": self.mode.as_str(),
            "inputs": self.inputs,
            "outputs": self.outputs,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceBodyProducer {
    PandocTex,
    LatexmlTex,
    RawTexFallback,
    PdfExtract,
    VlmAgent,
}

impl SourceBodyProducer {
    fn provenance(self, mode: ExtractionMode) -> NodeProvenance {
        match self {
            Self::PandocTex => NodeProvenance::cli_tool(
                "source_to_body",
                "pandoc_tex_to_markdown",
                ["source.tar.gz"],
                ["body.md"],
                mode,
            ),
            Self::LatexmlTex => NodeProvenance::cli_tool(
                "source_to_body",
                "latexml_tex_to_markdown",
                ["source.tar.gz"],
                ["body.md"],
                mode,
            ),
            Self::RawTexFallback => NodeProvenance::rust_tool(
                "source_to_body",
                "raw_tex_markdown_fallback",
                ["source.tar.gz"],
                ["body.md"],
                mode,
            ),
            Self::PdfExtract => NodeProvenance::rust_tool(
                "source_to_body",
                "pdf_extract_text",
                ["original.pdf"],
                ["body.md"],
                mode,
            ),
            Self::VlmAgent => NodeProvenance::agent(
                "vlm",
                "vlm_extractor",
                ["original.pdf"],
                ["body.md", "sections.json"],
                ExtractionMode::AgentEnabled,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactProvenance {
    artifact: String,
    producer_node: String,
    executor: ExecutorKind,
    tool: Option<String>,
    agent: Option<String>,
    mode: ExtractionMode,
    inputs: Vec<String>,
}

impl ArtifactProvenance {
    fn from_node(artifact: impl Into<String>, node: &NodeProvenance) -> Self {
        Self {
            artifact: artifact.into(),
            producer_node: node.node.clone(),
            executor: node.executor,
            tool: node.tool.clone(),
            agent: node.agent.clone(),
            mode: node.mode,
            inputs: node.inputs.clone(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "artifact": self.artifact,
            "producer_node": self.producer_node,
            "executor": self.executor.as_str(),
            "tool": self.tool,
            "agent": self.agent,
            "mode": self.mode.as_str(),
            "inputs": self.inputs,
        })
    }
}

/// Per-stage outcome captured from one extraction-agent run. The pipeline
/// preserves every field so the StageReport in `extraction_report.json`
/// has actionable provenance (model, runner, cost, latency, tool-call
/// summary) and the full per-call audit log can be uploaded to
/// Tier-2 storage.
#[derive(Debug, Clone)]
struct StageOutcome {
    output: Value,
    tool_calls: Vec<ToolCallRecord>,
    cost_usd: f32,
    latency_ms: i64,
    iters: u32,
    model: String,
    runner: String,
    provenance: NodeProvenance,
}

/// Per-paper-level options for [`run_ingest_pipeline`]. Built from the CLI's
/// global flags + RuntimeConfig.
#[derive(Debug, Default, Clone)]
pub struct IngestOptions {
    /// Force re-extraction even when `paper_assets.extraction_status='ready'`.
    pub no_cache: bool,
    /// Stage names to skip entirely (e.g. `["theorems", "citations"]`). Names
    /// match the keys used in `extraction_report.json` and the
    /// `--skip-stages` CLI flag.
    pub skip_stages: Vec<String>,
    /// Don't write Tier-2 (Supabase) artifacts. Tier-1 (Git) is still written
    /// to the local `grokrxiv-data` clone so the review path has a body.md.
    pub dry_run_storage: bool,
}

impl IngestOptions {
    fn should_skip(&self, stage: &str) -> bool {
        self.skip_stages
            .iter()
            .any(|s| s.eq_ignore_ascii_case(stage))
    }

    /// Build ingest options from the CLI-exported environment knobs.
    pub fn from_env() -> Self {
        let no_cache = matches!(
            std::env::var("GROKRXIV_INGEST_NO_CACHE").as_deref(),
            Ok("1")
        );
        let dry_run_storage = matches!(
            std::env::var("GROKRXIV_DRY_RUN_STORAGE").as_deref(),
            Ok("1")
        );
        let skip_stages: Vec<String> = std::env::var("GROKRXIV_INGEST_SKIP_STAGES")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            no_cache,
            skip_stages,
            dry_run_storage,
        }
    }
}

/// Result of one successful ingest pipeline run.
pub struct IngestResult {
    /// DB UUID of the paper row.
    pub paper_id: Uuid,
    /// Tier-1 / Tier-2 routing decisions for diagnostics.
    pub pointer: PersistedPointer,
    /// `review_input.json` payload — what the review DAG consumes.
    pub review_input: ReviewInput,
    /// `PaperExtract` reconstructed from the bundle so the review DAG can
    /// reason over `sections[*].body_markdown`. The orchestrator may also
    /// re-derive this from `review_input.json` via [`load_paper_extract`].
    pub extract: PaperExtract,
}

/// Drive Stages 1–8 for `arxiv_id`. Returns an [`IngestResult`] on success.
///
/// The function is idempotent on `(paper_id, extraction_status='ready')`: a
/// second call without `--no-cache` is a fast read of `review_input.json` +
/// `body.md`.
pub async fn run_ingest_pipeline(
    state: &AppState,
    arxiv_id: &str,
    opts: &IngestOptions,
) -> Result<IngestResult> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    // --- Stages 1+2: deterministic acquisition + format conversion ---
    let _permit = state.arxiv.acquire().await;
    let staged = {
        let s = grokrxiv_ingest::pipeline::ingest_staged(arxiv_id)
            .await
            .map_err(|e| anyhow::anyhow!("ingest_staged: {e}"))?;
        drop(_permit);
        s
    };
    let submitted_date = staged.meta.submitted_date;
    let paper_id = db::upsert_paper(pool, &staged.extract, submitted_date).await?;
    info!(arxiv_id, %paper_id, "Stage 1+2 complete");

    // --- Idempotent cache check ---
    if !opts.no_cache {
        if let Some(existing) = db::read_paper_assets(pool, paper_id).await? {
            if matches!(existing.extraction_status, db::ExtractionStatus::Ready) {
                if let Some(git_path) = existing.git_path.as_deref() {
                    info!(arxiv_id, %paper_id, git_path, "extraction_status=ready; short-circuiting");
                    return load_cached(paper_id, git_path, existing.storage_prefix.as_deref())
                        .await;
                }
            }
        }
    }

    // Mark running — concurrent ingests will see this via read_paper_assets.
    db::mark_paper_extracting(pool, paper_id).await?;

    let pipeline_started = Utc::now();
    let result = run_inner(state, pool, paper_id, arxiv_id, staged, opts).await;
    match result {
        Ok(out) => {
            info!(arxiv_id, %paper_id, elapsed_ms = (Utc::now() - pipeline_started).num_milliseconds(), "Stage 8 complete");
            Ok(out)
        }
        Err(e) => {
            let _ = db::mark_paper_extraction_failed(pool, paper_id, &format!("{e:#}")).await;
            Err(e)
        }
    }
}

async fn run_inner(
    state: &AppState,
    pool: &PgPool,
    paper_id: Uuid,
    arxiv_id: &str,
    staged: DeterministicIngest,
    opts: &IngestOptions,
) -> Result<IngestResult> {
    let started_at = Utc::now();
    let extraction_mode = extraction_mode_from_env()?;
    let workdir = tempfile::tempdir().context("creating extraction workdir")?;
    let extract = staged.extract.clone();
    let mut bundle = build_initial_bundle(&staged, &extract);
    let mut stage_reports: Vec<StageReport> = Vec::new();
    stage_reports.push(
        StageReport::ok("acquisition", None, None, Vec::new()).with_provenance(
            NodeProvenance::ingest_source(
                "acquire_source",
                "arxiv_metadata_source_pdf_fetch",
                ["arxiv_id"],
                ["metadata", "original.pdf", "source.tar.gz"],
                extraction_mode,
            ),
        ),
    );

    let mut source_body_producer = match staged.tex_body_producer {
        Some(IngestTexBodyProducer::PandocTex) => SourceBodyProducer::PandocTex,
        Some(IngestTexBodyProducer::LatexmlTex) => SourceBodyProducer::LatexmlTex,
        Some(IngestTexBodyProducer::RawTexFallback) => SourceBodyProducer::RawTexFallback,
        None => match extract.source_format.as_deref() {
            Some("tex") => SourceBodyProducer::PandocTex,
            Some("pdf") => SourceBodyProducer::PdfExtract,
            _ => SourceBodyProducer::PdfExtract,
        },
    };
    let body_md_str = bundle
        .body_markdown
        .clone()
        .unwrap_or_else(|| build_body_markdown(&extract));
    stage_reports.push(source_to_body_stage_report(
        source_body_producer,
        extraction_mode,
        &body_md_str,
    ));

    // Stage 2 reflection: Pandoc markdown is the default TeX path. LaTeXML is
    // optional enrichment for a semantic AST, so absence is only degraded when
    // the operator explicitly enabled LaTeXML.
    let semantic_ast = staged.semantic_ast;
    if semantic_ast.is_some() {
        stage_reports.push(
            StageReport::ok("tex_to_ast", None, None, Vec::new()).with_provenance(
                NodeProvenance::cli_tool(
                    "tex_to_ast",
                    "latexml_semantic_ast",
                    ["source.tar.gz"],
                    ["semantic_ast.json"],
                    extraction_mode,
                ),
            ),
        );
    } else if staged.source_tarball.is_some() && latexml_semantic_ast_enabled() {
        stage_reports.push(StageReport {
            name: "tex_to_ast".into(),
            status: "degraded".into(),
            duration_ms: None,
            cost_usd: None,
            model: None,
            runner: None,
            warnings: vec!["LaTeXML produced no semantic_ast".into()],
            iters: None,
            tool_call_summary: None,
            provenance: Some(NodeProvenance::cli_tool(
                "tex_to_ast",
                "latexml_semantic_ast",
                ["source.tar.gz"],
                ["semantic_ast.json"],
                extraction_mode,
            )),
        });
    } else if staged.source_tarball.is_some() {
        stage_reports.push(StageReport::skipped(
            "tex_to_ast",
            "LaTeXML semantic AST disabled; Pandoc body.md active",
        ));
    } else {
        stage_reports.push(StageReport::skipped("tex_to_ast", "no tex source"));
    }

    // Make the workdir useful to Stage 3 (VLM) by writing the PDF there.
    if let Some(bytes) = staged.pdf_bytes.as_ref() {
        let pdf_path = workdir.path().join(format!("{arxiv_id}.pdf"));
        let _ = std::fs::write(&pdf_path, bytes);
    }

    // Make the workdir useful to Stages 4-7 by unpacking the TeX bundle
    // when one exists.
    if let Some(tarball) = staged.source_tarball.as_ref() {
        if let Err(e) = unpack_tarball(workdir.path(), tarball) {
            warn!(error = %e, "tex tarball unpack failed; agents will see an empty workdir");
        }
    }

    // Make the rendered markdown available to Stages 4-7. Without this,
    // `list_citation_sites`, `extract_equations`, and `list_theorems` all
    // silently degrade because their tools read `workdir/body.md` and got
    // NoSuchFile. Written BEFORE Stages 4-7 fan out so all four parallel
    // agents see the file.
    write_body_md_to_workdir(workdir.path(), &body_md_str).await?;

    // semantic_ast embedded into the bundle for Tier-2 routing decisions.
    if let Some(ast) = semantic_ast.as_ref() {
        if let Ok(bytes) = serde_json::to_vec_pretty(ast) {
            bundle.semantic_ast = Some(bytes);
        }
    }

    // Accumulates the per-call audit log across every stage. Persisted as
    // `tool_call_log.jsonl` via `paper_artifacts.rs:297`.
    let mut tool_call_log_entries: Vec<Value> = Vec::new();

    // Concatenated TeX source from the unpacked workdir; used by the
    // semantic-success check for the macros stage to differentiate
    // "paper genuinely has no macros" from "tool failed to find them".
    let raw_tex_concat = read_workdir_tex_concat(workdir.path());
    let semantic_ctx = SemanticSuccessCtx {
        extract: &extract,
        raw_tex: raw_tex_concat.as_deref(),
    };

    // --- Stage 3 (VLM) — only when no TeX path was available ---
    if !opts.should_skip("vlm") && staged.source_tarball.is_none() {
        let vlm = VlmExtractorAgent::new();
        let outcome = run_agent_safe(
            &vlm,
            state,
            workdir.path(),
            &extract,
            semantic_ast.as_ref(),
            paper_id,
            arxiv_id,
            "vlm",
        )
        .await;
        if let Some(ref out) = outcome {
            apply_vlm(&mut bundle, &out.output);
            source_body_producer = SourceBodyProducer::VlmAgent;
        }
        push_tool_calls(&mut tool_call_log_entries, "vlm", outcome.as_ref());
        record_agent_outcome(
            &mut stage_reports,
            "vlm",
            outcome.as_ref(),
            opts,
            &semantic_ctx,
        );
    } else if staged.source_tarball.is_some() {
        stage_reports.push(StageReport::skipped("vlm", "tex path active"));
    }

    // --- Stages 4–7 ---
    //
    // Default CLI extraction is deterministic-first: get reviewer-useful
    // body/equation/citation/theorem artifacts locally, then reserve CLI LLM
    // tool loops for explicit opt-in. This keeps one paper from spending
    // minutes walking every citation key when deterministic citation contexts
    // already provide the reviewers' needed evidence.
    let semantic_ast_ref = semantic_ast.as_ref();
    let (macros_res, mut equations_res, mut theorems_res, mut citations_res) = if extraction_mode
        == ExtractionMode::AgentEnabled
    {
        tokio::join!(
            run_agent_when(
                !opts.should_skip("macros"),
                state,
                workdir.path(),
                &extract,
                semantic_ast_ref,
                paper_id,
                arxiv_id,
                "macros",
                MacroExpanderAgent::new(),
            ),
            run_agent_when(
                !opts.should_skip("equations"),
                state,
                workdir.path(),
                &extract,
                semantic_ast_ref,
                paper_id,
                arxiv_id,
                "equations",
                EquationCanonicalizerAgent::new(),
            ),
            run_agent_when(
                !opts.should_skip("theorems"),
                state,
                workdir.path(),
                &extract,
                semantic_ast_ref,
                paper_id,
                arxiv_id,
                "theorems",
                TheoremGraphExtractorAgent::new(),
            ),
            run_agent_when(
                !opts.should_skip("citations"),
                state,
                workdir.path(),
                &extract,
                semantic_ast_ref,
                paper_id,
                arxiv_id,
                "citations",
                CitationContextualizerAgent::new(),
            ),
        )
    } else {
        crate::cli_status::emit(format!(
                "extract {arxiv_id}: using pandoc_enabled local extraction; set GROKRXIV_EXTRACTION_MODE=agent_enabled to run LLM tool loops"
            ));
        (
            deterministic_macros_outcome(raw_tex_concat.as_deref()),
            deterministic_equations_or_empty(arxiv_id, semantic_ast_ref, &body_md_str),
            deterministic_theorems_or_empty(arxiv_id, &body_md_str),
            deterministic_citations_outcome(arxiv_id, &body_md_str, &extract),
        )
    };

    if should_use_deterministic_fallback("equations", equations_res.as_ref(), opts, &semantic_ctx) {
        equations_res = deterministic_equations_outcome(arxiv_id, semantic_ast_ref, &body_md_str);
    }
    if should_use_deterministic_fallback("theorems", theorems_res.as_ref(), opts, &semantic_ctx) {
        theorems_res = deterministic_theorems_outcome(arxiv_id, &body_md_str);
    }
    if should_use_citation_fallback(citations_res.as_ref(), opts, &body_md_str) {
        citations_res = deterministic_citations_outcome(arxiv_id, &body_md_str, &extract);
    } else {
        let deterministic_citations =
            deterministic_citations_outcome(arxiv_id, &body_md_str, &extract);
        if should_replace_low_quality_citations(
            citations_res.as_ref(),
            deterministic_citations.as_ref(),
        ) {
            citations_res = deterministic_citations;
        }
    }

    push_tool_calls(&mut tool_call_log_entries, "macros", macros_res.as_ref());
    push_tool_calls(
        &mut tool_call_log_entries,
        "equations",
        equations_res.as_ref(),
    );
    push_tool_calls(
        &mut tool_call_log_entries,
        "theorems",
        theorems_res.as_ref(),
    );
    push_tool_calls(
        &mut tool_call_log_entries,
        "citations",
        citations_res.as_ref(),
    );

    record_agent_outcome(
        &mut stage_reports,
        "macros",
        macros_res.as_ref(),
        opts,
        &semantic_ctx,
    );
    record_agent_outcome(
        &mut stage_reports,
        "equations",
        equations_res.as_ref(),
        opts,
        &semantic_ctx,
    );
    record_agent_outcome(
        &mut stage_reports,
        "theorems",
        theorems_res.as_ref(),
        opts,
        &semantic_ctx,
    );
    record_agent_outcome(
        &mut stage_reports,
        "citations",
        citations_res.as_ref(),
        opts,
        &semantic_ctx,
    );

    let artifact_provenance = build_artifact_provenance(
        source_body_producer,
        extraction_mode,
        equations_res.as_ref(),
        theorems_res.as_ref(),
        citations_res.as_ref(),
    );

    apply_equations(&mut bundle, arxiv_id, equations_res);
    apply_theorems(&mut bundle, arxiv_id, theorems_res);
    apply_citations(&mut bundle, arxiv_id, citations_res);
    if let Some(references) = &bundle.references {
        bundle.citation_validation_report = Some(build_citation_validation_report(references));
    }
    apply_macros(&mut bundle, macros_res); // currently records to extraction_report

    // Serialize the cross-stage tool_call log as JSONL for Tier-2 audit.
    if !tool_call_log_entries.is_empty() {
        let jsonl = tool_call_log_entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n");
        bundle.tool_call_log = Some(jsonl.into_bytes());
    }

    // --- Stage 8: persist + finalise ---
    let completed_at = Utc::now();
    let report = json!({
        "arxiv_id": arxiv_id,
        "mode": extraction_mode.as_str(),
        "started_at": started_at.to_rfc3339(),
        "completed_at": completed_at.to_rfc3339(),
        "total_cost_usd": null,
        "stages": stage_reports.iter().map(StageReport::to_value).collect::<Vec<_>>(),
        "artifact_provenance": artifact_provenance.iter().map(ArtifactProvenance::to_value).collect::<Vec<_>>(),
    });
    bundle.extraction_report = Some(report);

    let stages_run: Vec<&str> = stage_reports
        .iter()
        .filter(|s| s.status == "ok")
        .map(|s| s.name.as_str())
        .collect();

    let paper_artifacts = build_paper_artifacts(opts)?;
    let pointer = paper_artifacts
        .persist(paper_id.to_string(), bundle.clone(), &stages_run, None)
        .await
        .context("PaperArtifacts::persist (Stage 8)")?;
    db::persist_paper_extraction(
        pool,
        paper_id,
        &pointer.git_path,
        pointer.git_commit_sha.as_deref(),
        &pointer.storage_prefix,
        pointer.extraction_cost_usd,
    )
    .await?;

    let review_input = bundle.to_review_input(false);
    Ok(IngestResult {
        paper_id,
        pointer,
        review_input,
        extract,
    })
}

/// Build a [`PaperArtifacts`] router from environment + per-run opts.
///
/// Env knobs:
/// - `GROKRXIV_DATA_REPO_PATH` (required) — local path to the
///   `grokrxiv-data` clone. Defaults to
///   `/Users/mlong/Documents/Development/grokrxiv-data` if absent.
/// - `GROKRXIV_DATA_REPO_REMOTE` (optional) — git remote URL; when unset the
///   local repo's existing remote is used (or no push happens).
/// - `SUPABASE_URL` + `SUPABASE_SERVICE_ROLE_KEY` (optional) — Tier-2 writes
///   are silently skipped when either is absent.
/// - `GROKRXIV_DRY_RUN_STORAGE=1` or `opts.dry_run_storage = true` — skip
///   Tier 2 even when env vars are present.
fn build_paper_artifacts(opts: &IngestOptions) -> Result<PaperArtifacts> {
    let repo_path: PathBuf = std::env::var("GROKRXIV_DATA_REPO_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/mlong/Documents/Development/grokrxiv-data"));
    let remote = std::env::var("GROKRXIV_DATA_REPO_REMOTE").ok();
    let git =
        GitArtifactStore::open_or_clone(repo_path, remote).context("open grokrxiv-data repo")?;

    let dry_run = opts.dry_run_storage
        || matches!(
            std::env::var("GROKRXIV_DRY_RUN_STORAGE").as_deref(),
            Ok("1")
        );
    let storage = if dry_run {
        None
    } else {
        match (
            std::env::var("SUPABASE_URL"),
            std::env::var("SUPABASE_SERVICE_ROLE_KEY"),
        ) {
            (Ok(url), Ok(key)) => Some(SupabaseStorage::new(url, key)),
            _ => {
                info!("SUPABASE_URL or SUPABASE_SERVICE_ROLE_KEY missing — skipping Tier 2 writes");
                None
            }
        }
    };
    Ok(PaperArtifacts::new(git, storage))
}

fn build_initial_bundle(staged: &DeterministicIngest, extract: &PaperExtract) -> ArtifactBundle {
    let arxiv_id = &staged.meta.arxiv_id;
    let metadata = build_metadata_json(staged, extract);
    let sections = build_sections_json(arxiv_id, extract);
    let body = build_body_markdown(extract);
    let references = build_initial_references_json(arxiv_id, extract);
    let mut b = ArtifactBundle::new(arxiv_id);
    b.metadata = Some(metadata);
    b.sections = Some(sections);
    b.body_markdown = Some(body);
    b.references = Some(references);
    b.original_pdf = staged.pdf_bytes.as_ref().map(|x| x.to_vec());
    b.source_tarball = staged.source_tarball.as_ref().map(|x| x.to_vec());
    if let Some(tarball) = staged.source_tarball.as_ref() {
        match build_source_manifest_json(arxiv_id, tarball) {
            Ok(manifest) => b.source_manifest = Some(manifest),
            Err(e) => warn!(error = %e, "source manifest extraction failed"),
        }
        match extract_source_figures(tarball) {
            Ok(figures) => b.figures = figures,
            Err(e) => warn!(error = %e, "source figure extraction failed"),
        }
    }
    // Initial empty placeholders so equations.json + theorem_graph.json
    // always validate against their respective schemas even when Stages 5/6
    // are skipped or fail.
    b.equations = Some(json!({ "arxiv_id": arxiv_id, "equations": [] }));
    b.theorem_graph = Some(json!({ "arxiv_id": arxiv_id, "nodes": [] }));
    b
}

fn build_metadata_json(staged: &DeterministicIngest, extract: &PaperExtract) -> Value {
    let mut authors: Vec<Value> = extract
        .authors
        .iter()
        .map(|a| {
            json!({
                "name": a.name,
                "affiliation": a.affiliation,
                "orcid": Value::Null,
            })
        })
        .collect();
    if authors.is_empty() {
        // metadata.schema.json requires `authors` to be an array; allow empty.
        authors = Vec::new();
    }
    json!({
        "arxiv_id": extract.arxiv_id,
        "title": extract.title,
        "authors": authors,
        "abstract": extract.abstract_,
        "doi": Value::Null,
        "submitted_date": staged.meta.submitted_date.map(|d| d.to_string()),
        "updated_date": Value::Null,
        "primary_category": extract.field,
        "categories": staged.meta.categories,
        "version": Value::Null,
        "license": Value::Null,
    })
}

fn build_sections_json(arxiv_id: &str, extract: &PaperExtract) -> Value {
    // Compute char offsets against the assembled body.md so downstream
    // consumers can slice it.
    let mut sections: Vec<Value> = Vec::with_capacity(extract.sections.len());
    let mut cursor: i64 = 0;
    for (idx, s) in extract.sections.iter().enumerate() {
        let heading = format!("## {}\n\n", s.heading);
        let body = format!("{}\n\n", s.body_markdown);
        let char_start = cursor + heading.len() as i64;
        let char_end = char_start + body.trim_end().len() as i64;
        cursor = char_start + body.len() as i64;
        sections.push(json!({
            "id": format!("s{idx}"),
            "heading": s.heading,
            "level": 2,
            "char_start": char_start,
            "char_end": char_end,
            "parent_id": Value::Null,
        }));
    }
    json!({ "arxiv_id": arxiv_id, "sections": sections })
}

fn build_body_markdown(extract: &PaperExtract) -> String {
    let mut out = String::new();
    for s in &extract.sections {
        out.push_str("## ");
        out.push_str(&s.heading);
        out.push_str("\n\n");
        out.push_str(&s.body_markdown);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn build_initial_references_json(arxiv_id: &str, extract: &PaperExtract) -> Value {
    let citations: Vec<Value> = extract
        .bibliography
        .iter()
        .enumerate()
        .map(|(i, c)| {
            // references.schema requires `key`. Synthesise one when not parsed.
            let key = citation_key(c).unwrap_or_else(|| format!("ref{i}"));
            json!({
                "key": key,
                "raw": c.raw,
                "title": c.title,
                "authors": Vec::<String>::new(),
                "venue": Value::Null,
                "year": Value::Null,
                "doi": c.doi,
                "arxiv_id": c.arxiv_id,
                "cited": false,
                "contexts": Vec::<Value>::new(),
                "validation": Value::Null,
            })
        })
        .collect();
    json!({
        "arxiv_id": arxiv_id,
        "citations": citations,
        "unmatched_citation_keys": Vec::<String>::new(),
        "uncited_bibliography_keys": Vec::<String>::new(),
    })
}

fn unpack_tarball(workdir: &Path, bytes: &[u8]) -> Result<()> {
    for entry in read_source_archive_entries(bytes).context("read source archive entries")? {
        let target = workdir.join(&entry.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create source archive directory {parent:?}"))?;
        }
        std::fs::write(&target, &entry.bytes)
            .with_context(|| format!("write source archive entry {target:?}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct SourceArchiveEntry {
    path: String,
    bytes: Vec<u8>,
}

fn build_source_manifest_json(arxiv_id: &str, bytes: &[u8]) -> Result<Value> {
    let mut entries: Vec<Value> = read_source_archive_entries(bytes)?
        .into_iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "size": entry.bytes.len() as u64,
                "sha256": sha256_hex(&entry.bytes),
                "kind": source_entry_kind(&entry.path),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a.get("path")
            .and_then(Value::as_str)
            .cmp(&b.get("path").and_then(Value::as_str))
    });
    Ok(json!({
        "arxiv_id": arxiv_id,
        "tarball_sha256": sha256_hex(bytes),
        "entries": entries,
    }))
}

fn extract_source_figures(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut figures: Vec<(String, Vec<u8>)> = read_source_archive_entries(bytes)?
        .into_iter()
        .filter(|entry| is_figure_path(&entry.path))
        .map(|entry| (entry.path, entry.bytes))
        .collect();
    figures.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(figures)
}

fn read_source_archive_entries(bytes: &[u8]) -> Result<Vec<SourceArchiveEntry>> {
    read_source_archive_entries_targz(bytes).or_else(|_| read_source_archive_entries_tar(bytes))
}

fn read_source_archive_entries_targz(bytes: &[u8]) -> Result<Vec<SourceArchiveEntry>> {
    use std::io::Cursor;
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    read_tar_entries(decoder)
}

fn read_source_archive_entries_tar(bytes: &[u8]) -> Result<Vec<SourceArchiveEntry>> {
    use std::io::Cursor;
    read_tar_entries(Cursor::new(bytes))
}

fn read_tar_entries<R: std::io::Read>(reader: R) -> Result<Vec<SourceArchiveEntry>> {
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries().context("read source archive entries")? {
        let mut entry = entry.context("read source archive entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().context("read source archive entry path")?;
        let Some(path) = safe_archive_path(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .context("read source archive entry bytes")?;
        out.push(SourceArchiveEntry { path, bytes });
    }
    Ok(out)
}

fn safe_archive_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().to_string());
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    let rel = parts.join("/");
    (!rel.is_empty()).then_some(rel)
}

fn source_entry_kind(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".tex") {
        "tex"
    } else if lower.ends_with(".bib") {
        "bib"
    } else if lower.ends_with(".bbl") {
        "bbl"
    } else if lower.ends_with(".sty") {
        "sty"
    } else if lower.ends_with(".cls") {
        "cls"
    } else if is_figure_path(path) {
        "figure"
    } else if lower.ends_with(".pdf") {
        "pdf"
    } else {
        "other"
    }
}

fn is_figure_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    matches!(
        Path::new(&lower).extension().and_then(|ext| ext.to_str()),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "eps")
    ) || (lower.ends_with(".pdf")
        && lower
            .split('/')
            .any(|segment| segment == "fig" || segment == "figs" || segment == "figures"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Read every `*.tex` file in the workdir and return their concatenation.
/// Used by the FP-RPT3a A4 semantic-success check for the macros stage —
/// if the concatenated source contains any `\newcommand` / `\def` /
/// `\renewcommand` / `\DeclareMathOperator` but the agent submitted
/// `expansions_applied: []`, we flag the stage as degraded.
fn read_workdir_tex_concat(workdir: &Path) -> Option<String> {
    let mut out = String::new();
    read_tex_recursive(workdir, &mut out).ok()?;
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn read_tex_recursive(dir: &Path, out: &mut String) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            read_tex_recursive(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("tex") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                out.push_str(&contents);
                out.push('\n');
            }
        }
    }
    Ok(())
}

/// Write the rendered `body.md` into the extraction workdir so Stages 4-7's
/// tools (`list_citation_sites`, `extract_equations` fallback, `list_sections`,
/// `read_section`) can read it. Exposed so a regression test can confirm the
/// file ends up on disk where the tools expect it.
pub async fn write_body_md_to_workdir(workdir: &Path, body_md: &str) -> Result<()> {
    tokio::fs::write(workdir.join("body.md"), body_md)
        .await
        .context("write body.md to extraction workdir")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent runner wrappers
// ---------------------------------------------------------------------------

fn resolve_runner(
    state: &AppState,
    stage: &str,
) -> Option<(Arc<dyn AgentRunner>, AgentRunnerKind)> {
    let extractor = resolve_extractor(stage);
    let kind = extractor.runner_kind();
    state
        .runners
        .get(&kind)
        .cloned()
        .map(|runner| (runner, kind))
}

fn resolve_extractor(stage: &str) -> ExtractorKind {
    let extractor = std::env::var("AGENTHERO_EXTRACTOR").ok();
    let fallback = std::env::var("AGENTHERO_EXTRACTION_TOOL_FALLBACK").ok();
    resolve_extractor_from_routing(
        stage,
        extractor.as_deref(),
        fallback.as_deref(),
        load_extraction_routing(stage).as_ref(),
    )
}

fn resolve_extractor_from_routing(
    stage: &str,
    extractor: Option<&str>,
    fallback: Option<&str>,
    routing: Option<&ExtractionRouting>,
) -> ExtractorKind {
    if let Some(v) = extractor {
        if let Some(kind) = parse_extractor(v) {
            return kind;
        }
        tracing::warn!(
            stage,
            value = %v,
            "invalid AGENTHERO_EXTRACTOR; falling back to CLI extraction"
        );
        return ExtractorKind::Cli;
    }
    if matches!(fallback, Some("api")) {
        return ExtractorKind::Api;
    }
    routing.and_then(|r| r.runner).unwrap_or_default()
}

fn extraction_mode_from_env() -> Result<ExtractionMode> {
    if std::env::var("GROKRXIV_FORCE_AGENT_EXTRACTION").is_ok() {
        return Err(anyhow!(
            "GROKRXIV_FORCE_AGENT_EXTRACTION is no longer supported; use \
             GROKRXIV_EXTRACTION_MODE=agent_enabled or pandoc_enabled"
        ));
    }

    match std::env::var("GROKRXIV_EXTRACTION_MODE") {
        Ok(value) => match value.as_str() {
            "pandoc_enabled" => Ok(ExtractionMode::PandocEnabled),
            "agent_enabled" => Ok(ExtractionMode::AgentEnabled),
            other => Err(anyhow!(
                "invalid GROKRXIV_EXTRACTION_MODE={other:?}; expected pandoc_enabled or agent_enabled"
            )),
        },
        Err(std::env::VarError::NotPresent) => Ok(ExtractionMode::PandocEnabled),
        Err(e) => Err(anyhow!("read GROKRXIV_EXTRACTION_MODE: {e}")),
    }
}

fn env_truthy(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

fn latexml_semantic_ast_enabled() -> bool {
    env_truthy("GROKRXIV_TEX_ENABLE_LATEXML") && !env_truthy("GROKRXIV_TEX_DISABLE_LATEXML")
}

/// Per-stage routing read from `agents/extraction/<role>.yaml`. Captures the
/// fields the extraction tool-loop needs at runtime.
#[derive(serde::Deserialize, Clone, Debug)]
struct ExtractionRouting {
    provider: String,
    model: String,
    #[serde(default)]
    runner: Option<ExtractorKind>,
    #[serde(default)]
    max_cost_usd: Option<f32>,
    #[serde(default)]
    max_iters: Option<u32>,
    /// `macros.yaml` nests budgets under `loop: {max_iters, max_cost_usd}`;
    /// the others put them at the top level. Accept either form.
    #[serde(default, rename = "loop")]
    loop_block: Option<ExtractionRoutingLoop>,
    #[serde(default)]
    timeout_secs: Option<u32>,
    #[serde(default)]
    max_retries: Option<u8>,
}

#[derive(serde::Deserialize, Clone, Debug)]
struct ExtractionRoutingLoop {
    #[serde(default)]
    max_cost_usd: Option<f32>,
    #[serde(default)]
    max_iters: Option<u32>,
}

impl ExtractionRouting {
    fn resolved_max_cost_usd(&self) -> Option<f32> {
        self.max_cost_usd
            .or_else(|| self.loop_block.as_ref().and_then(|l| l.max_cost_usd))
    }
    fn resolved_max_iters(&self) -> Option<u32> {
        self.max_iters
            .or_else(|| self.loop_block.as_ref().and_then(|l| l.max_iters))
    }
}

/// Load extraction routing through `dags/paper-extract.yaml`, falling back to
/// the legacy `agents/extraction/<stage>.yaml` path. Lazy-cached per-stage so
/// repeated agent invocations within a single ingest don't re-read files.
fn load_extraction_routing(stage: &str) -> Option<ExtractionRouting> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, Option<ExtractionRouting>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.get(stage) {
            return cached.clone();
        }
    }

    let path = extraction_routing_path(stage);
    let parsed = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_yaml::from_str::<ExtractionRouting>(&s) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(
                    stage,
                    path = %path.display(),
                    err = %e,
                    "could not parse extraction yaml; falling back to default"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                stage,
                path = %path.display(),
                err = %e,
                "extraction yaml missing; falling back to default"
            );
            None
        }
    };

    if let Ok(mut guard) = cache.lock() {
        guard.insert(stage.to_string(), parsed.clone());
    }
    parsed
}

fn extraction_routing_path(stage: &str) -> std::path::PathBuf {
    if let Some(path) = extraction_routing_path_from_manifest(stage) {
        return path;
    }
    let agents_dir = std::env::var("AGENTHERO_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("agents"));
    agents_dir.join("extraction").join(format!("{stage}.yaml"))
}

fn extraction_routing_path_from_manifest(stage: &str) -> Option<std::path::PathBuf> {
    let role_id = extraction_manifest_role_id(stage)?;
    let dags_dir = std::env::var("AGENTHERO_DAGS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let cwd_dags = cwd.join("dags");
            if cwd_dags.is_dir() {
                cwd_dags
            } else {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("dags")
            }
        });
    let manifest_path = dags_dir.join("paper-extract.yaml");
    let manifest = agenthero_dag_runtime::DagManifest::from_path(&manifest_path).ok()?;
    let config = manifest
        .roles
        .iter()
        .find(|role| role.id.as_str() == role_id)?
        .config
        .as_deref()?;
    let config_path = std::path::PathBuf::from(config);
    if config_path.is_absolute() {
        Some(config_path)
    } else if let Some(agents_dir) = std::env::var_os("AGENTHERO_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .and_then(|agents_dir| {
            config_path
                .strip_prefix("agents")
                .ok()
                .map(|stripped| agents_dir.join(stripped))
        })
    {
        Some(agents_dir)
    } else {
        Some(dags_dir.parent()?.join(config_path))
    }
}

fn extraction_manifest_role_id(stage: &str) -> Option<&'static str> {
    match stage {
        "vlm" => Some("vlm_extractor"),
        "macros" => Some("macro_expander"),
        "equations" => Some("equation_canonicalizer"),
        "theorems" => Some("theorem_graph_extractor"),
        "citations" => Some("citation_contextualizer"),
        _ => None,
    }
}

/// Per-stage budget bundle (resolved from YAML with fallbacks). Surfaced
/// separately from `AgentSpec` because the tool-loop's cost/iter ceilings live
/// on its `run_tool_loop` args, not on the spec.
pub(crate) struct ExtractionBudget {
    pub max_cost_usd: f32,
    pub max_iters: u32,
}

/// Per-stage budget bundle (resolved from YAML with fallbacks). FP-RPT3a A5
/// threads these into `ExtractionContext` so each agent's `run()` can honour
/// the YAML number instead of its hardcoded inline ceiling.
pub(crate) fn extraction_budget_for(stage: &str) -> ExtractionBudget {
    let routing = load_extraction_routing(stage);
    extraction_budget_from_routing(stage, routing.as_ref())
}

fn extraction_budget_from_routing(
    stage: &str,
    routing: Option<&ExtractionRouting>,
) -> ExtractionBudget {
    let (cost, iters) = match stage {
        "vlm" => (1.0, 40),
        "macros" => (0.40, 20),
        "equations" => (0.50, 60),
        "theorems" => (1.50, 50),
        "citations" => (0.50, 80),
        _ => (0.50, 40),
    };
    ExtractionBudget {
        max_cost_usd: routing
            .and_then(|r| r.resolved_max_cost_usd())
            .unwrap_or(cost),
        max_iters: routing
            .and_then(|r| r.resolved_max_iters())
            .unwrap_or(iters),
    }
}

fn default_extraction_spec(role: &str, runner_kind: AgentRunnerKind) -> AgentSpec {
    let routing = load_extraction_routing(role);
    let (provider, model) = match routing.as_ref() {
        Some(r) => (r.provider.clone(), r.model.clone()),
        None => (
            "claude".to_string(),
            std::env::var("GROKRXIV_EXTRACTION_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5".to_string()),
        ),
    };
    let mut spec = AgentSpec::api_default(role.to_string(), provider, model);
    spec.runner = runner_kind;
    if let Some(r) = routing {
        if let Some(t) = r.timeout_secs {
            spec.timeout_secs = t;
        }
        if let Some(n) = r.max_retries {
            spec.max_retries = n;
        }
    }
    spec
}

fn extraction_agent_inputs(stage_name: &str) -> Vec<&'static str> {
    match stage_name {
        "vlm" => vec!["original.pdf"],
        "macros" => vec!["source.tar.gz", "body.md"],
        "equations" => vec!["body.md", "semantic_ast.json"],
        "theorems" => vec!["body.md", "semantic_ast.json"],
        "citations" => vec!["body.md", "references.json"],
        _ => vec!["body.md"],
    }
}

fn extraction_agent_outputs(stage_name: &str) -> Vec<&'static str> {
    match stage_name {
        "vlm" => vec!["body.md", "sections.json", "vlm_raw.json"],
        "macros" => vec!["extraction_report.json"],
        "equations" => vec!["equations.json"],
        "theorems" => vec!["theorem_graph.json"],
        "citations" => vec!["references.json"],
        _ => vec!["extraction_report.json"],
    }
}

/// Run an extraction agent end-to-end, swallowing any error so a single
/// stage's failure can't tank the whole pipeline. Logs at `warn` on error and
/// returns `None`; logs `output` size on success.
async fn run_agent_safe<A: ExtractionAgent + Sized + 'static>(
    agent: &A,
    state: &AppState,
    workdir: &Path,
    extract: &PaperExtract,
    semantic_ast: Option<&Value>,
    paper_id: Uuid,
    arxiv_id: &str,
    stage_name: &str,
) -> Option<StageOutcome> {
    let Some((runner, runner_kind)) = resolve_runner(state, stage_name) else {
        warn!(stage_name, "no runner available; skipping extraction stage");
        return None;
    };
    let runner_name = runner.name().to_string();
    let registry = Arc::new(build_registry_for(agent));
    let budget = extraction_budget_for(stage_name);
    let ctx = ExtractionContext {
        workdir,
        extract,
        semantic_ast,
        paper_id,
        arxiv_id,
        registry,
        max_cost_usd: budget.max_cost_usd,
        max_iters: budget.max_iters,
    };
    info!(
        stage_name,
        max_cost_usd = budget.max_cost_usd,
        max_iters = budget.max_iters,
        "extraction budget resolved from agents/extraction/<stage>.yaml",
    );
    let spec = default_extraction_spec(stage_name, runner_kind);
    let model = spec.model.clone();
    crate::cli_status::emit(format!(
        "extract {arxiv_id}: {stage_name} starting via {runner_name} ({model})"
    ));
    match agent.run(runner, &spec, ctx).await {
        Ok(run) => {
            info!(
                stage_name,
                arxiv_id,
                iters = run.iters,
                "extraction stage ok"
            );
            crate::cli_status::emit(format!(
                "extract {arxiv_id}: {stage_name} ok iters={}",
                run.iters
            ));
            Some(StageOutcome {
                output: run.output,
                tool_calls: run.tool_calls,
                cost_usd: run.cost_usd,
                latency_ms: run.latency_ms,
                iters: run.iters,
                model,
                runner: runner_name,
                provenance: NodeProvenance::agent(
                    stage_name,
                    agent.name(),
                    extraction_agent_inputs(stage_name),
                    extraction_agent_outputs(stage_name),
                    ExtractionMode::AgentEnabled,
                ),
            })
        }
        Err(e) => {
            warn!(stage_name, arxiv_id, err = %format!("{e:#}"), "extraction stage failed");
            crate::cli_status::emit(format!(
                "extract {arxiv_id}: {stage_name} failed; deterministic fallback may run"
            ));
            None
        }
    }
}

/// Variant of [`run_agent_safe`] that short-circuits to `None` when the stage
/// is skipped (via `--skip-stages` or because there's no source to run it
/// against). Keeps the `tokio::join!` block tidy.
async fn run_agent_when<A: ExtractionAgent + Sized + 'static>(
    enabled: bool,
    state: &AppState,
    workdir: &Path,
    extract: &PaperExtract,
    semantic_ast: Option<&Value>,
    paper_id: Uuid,
    arxiv_id: &str,
    stage_name: &str,
    agent: A,
) -> Option<StageOutcome> {
    if !enabled {
        return None;
    }
    run_agent_safe(
        &agent,
        state,
        workdir,
        extract,
        semantic_ast,
        paper_id,
        arxiv_id,
        stage_name,
    )
    .await
}

/// Build a registry per agent. The framework requires the `ToolRegistry`
/// inside `ExtractionContext` to carry every tool the agent's
/// `submit`/`call` loop will invoke. Each concrete agent type knows its own
/// tool set — we dispatch by type id.
fn build_registry_for<A: ExtractionAgent + Sized + 'static>(_agent: &A) -> ToolRegistry {
    use std::any::TypeId;
    let id = TypeId::of::<A>();
    if id == TypeId::of::<VlmExtractorAgent>() {
        grokrxiv_extraction::extraction::vlm::build_registry()
    } else if id == TypeId::of::<MacroExpanderAgent>() {
        MacroExpanderAgent::new().registry()
    } else if id == TypeId::of::<EquationCanonicalizerAgent>() {
        EquationCanonicalizerAgent::registry()
    } else if id == TypeId::of::<TheoremGraphExtractorAgent>() {
        TheoremGraphExtractorAgent::build_registry()
    } else {
        // Fall back to the core toolkit. The citations agent advertises core
        // tools (crossref_lookup, arxiv_lookup) plus its own — we
        // re-register the per-agent ones on top.
        let mut r = ToolRegistry::with_core_tools();
        r.register(Arc::new(
            grokrxiv_extraction::extraction::citations::tools::ListCitationSitesTool,
        ));
        r.register(Arc::new(
            grokrxiv_extraction::extraction::citations::tools::LookupBibtexTool,
        ));
        r.register(Arc::new(
            grokrxiv_extraction::extraction::citations::tools::SearchCorpusTool,
        ));
        r.register(Arc::new(
            grokrxiv_extraction::extraction::citations::tools::ReadSectionTool,
        ));
        r
    }
}

// ---------------------------------------------------------------------------
// ArtifactBundle apply helpers
// ---------------------------------------------------------------------------

fn apply_vlm(bundle: &mut ArtifactBundle, output: &Value) {
    if let Some(sections) = output.get("sections").and_then(|v| v.as_array()) {
        let mut body = String::new();
        let mut sec_entries: Vec<Value> = Vec::new();
        let mut cursor: i64 = 0;
        for (idx, s) in sections.iter().enumerate() {
            let heading = s
                .get("heading")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let body_md = s
                .get("body_markdown")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let h_block = format!("## {heading}\n\n");
            let b_block = format!("{body_md}\n\n");
            let char_start = cursor + h_block.len() as i64;
            let char_end = char_start + b_block.trim_end().len() as i64;
            cursor = char_start + b_block.len() as i64;
            body.push_str(&h_block);
            body.push_str(&b_block);
            sec_entries.push(json!({
                "id": format!("s{idx}"),
                "heading": heading,
                "level": 2,
                "char_start": char_start,
                "char_end": char_end,
                "parent_id": Value::Null,
            }));
        }
        bundle.body_markdown = Some(body);
        bundle.sections = Some(json!({ "arxiv_id": bundle.arxiv_id, "sections": sec_entries }));
    }
    // Stash the raw VLM payload for Tier-2 audit.
    if let Ok(bytes) = serde_json::to_vec_pretty(output) {
        bundle.vlm_raw = Some(bytes);
    }
}

fn apply_equations(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<StageOutcome>) {
    let Some(value) = res.map(|o| o.output) else {
        return;
    };
    // The agent returns `{equations: [...]}`; references.schema requires
    // arxiv_id at the top level. Inject + propagate when the agent didn't.
    let equations = value
        .get("equations")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    // Normalise: every entry must satisfy equations.schema.json.
    let canonical: Vec<Value> = equations
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e.get("id")?.as_str()?.to_string();
                    let canonical_tex = e
                        .get("canonical_tex")
                        .or_else(|| e.get("tex"))
                        .and_then(Value::as_str)?
                        .to_string();
                    Some(json!({
                        "id": id,
                        "canonical_tex": canonical_tex,
                        "mathml": e.get("mathml").cloned().unwrap_or(Value::Null),
                        "semantic_tag": e.get("semantic_tag").cloned().unwrap_or(Value::Null),
                        "section_id": e.get("section_id").cloned().unwrap_or(Value::Null),
                        "hash": e.get("hash").cloned().unwrap_or(Value::Null),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    bundle.equations = Some(json!({ "arxiv_id": arxiv_id, "equations": canonical }));
}

fn apply_theorems(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<StageOutcome>) {
    let Some(value) = res.map(|o| o.output) else {
        return;
    };
    // Agent may return `theorem_graph: [...]` or `nodes: [...]`. The
    // theorem_graph.schema requires `{arxiv_id, nodes: [...]}`.
    let nodes_src = value
        .get("nodes")
        .cloned()
        .or_else(|| value.get("theorem_graph").cloned())
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let nodes: Vec<Value> = nodes_src
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|n| {
                    let id = n.get("id")?.as_str()?.to_string();
                    let node_type = n.get("type")?.as_str()?.to_string();
                    let statement = n.get("statement")?.as_str()?.to_string();
                    let depends_on: Vec<Value> = n
                        .get("depends_on")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    Some(json!({
                        "id": id,
                        "type": node_type,
                        "statement": statement,
                        "section_id": n.get("section_id").cloned().or_else(|| n.get("section").cloned()).unwrap_or(Value::Null),
                        "depends_on": depends_on,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    bundle.theorem_graph = Some(json!({ "arxiv_id": arxiv_id, "nodes": nodes }));
}

fn apply_citations(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<StageOutcome>) {
    let Some(value) = res.map(|o| o.output) else {
        return;
    };
    let citations_src = value
        .get("citations")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let citations: Vec<Value> = citations_src
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let key = c.get("key")?.as_str()?.to_string();
                    // references.schema requires `key`; the other fields are optional.
                    let contexts: Vec<Value> = c
                        .get("contexts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|cx| {
                            // references.schema's `use` allows arbitrary
                            // strings; the citations agent already constrains
                            // it via its own enum. Pass through.
                            let section = cx.get("section")?.as_str()?.to_string();
                            let sentence = cx.get("sentence")?.as_str()?.to_string();
                            let use_ = cx.get("use")?.as_str()?.to_string();
                            Some(json!({
                                "section": section,
                                "sentence": sentence,
                                "use": use_,
                            }))
                        })
                        .collect();
                    Some(json!({
                        "key": key,
                        "raw": c.get("raw").cloned().unwrap_or(Value::Null),
                        "title": c.get("title").cloned().unwrap_or(Value::Null),
                        "authors": c.get("authors").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
                        "venue": c.get("venue").cloned().unwrap_or(Value::Null),
                        "year": c.get("year").cloned().unwrap_or(Value::Null),
                        "doi": c.get("doi").or_else(|| c.get("resolved_doi")).cloned().unwrap_or(Value::Null),
                        "arxiv_id": c.get("arxiv_id").or_else(|| c.get("resolved_arxiv_id")).cloned().unwrap_or(Value::Null),
                        "cited": c.get("cited").cloned().unwrap_or_else(|| Value::Bool(!contexts.is_empty())),
                        "contexts": contexts,
                        "validation": c.get("validation").cloned().unwrap_or(Value::Null),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    bundle.references = Some(json!({
        "arxiv_id": arxiv_id,
        "citations": citations,
        "unmatched_citation_keys": value_string_array(&value, "unmatched_citation_keys"),
        "uncited_bibliography_keys": value_string_array(&value, "uncited_bibliography_keys"),
    }));
}

fn value_string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn build_citation_validation_report(references: &Value) -> Value {
    let citations = references
        .get("citations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let unmatched = value_string_array(references, "unmatched_citation_keys");
    let uncited = value_string_array(references, "uncited_bibliography_keys");

    let parsed_references: Vec<Value> = citations
        .iter()
        .map(|citation| {
            json!({
                "key": citation.get("key").and_then(Value::as_str).unwrap_or("").to_string(),
                "raw": citation.get("raw").and_then(Value::as_str).unwrap_or("").to_string(),
                "title": citation.get("title").cloned().unwrap_or(Value::Null),
                "authors": citation.get("authors").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
                "venue": citation.get("venue").cloned().unwrap_or(Value::Null),
                "year": citation.get("year").cloned().unwrap_or(Value::Null),
                "doi": citation.get("doi").cloned().unwrap_or(Value::Null),
                "arxiv_id": citation.get("arxiv_id").cloned().unwrap_or(Value::Null),
                "cited": citation.get("cited").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect();

    let resolver_results: Vec<Value> = citations
        .iter()
        .map(|citation| {
            let key = citation
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let validation = citation.get("validation").unwrap_or(&Value::Null);
            let raw_status = validation
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    if citation.get("doi").and_then(Value::as_str).is_some()
                        || citation.get("arxiv_id").and_then(Value::as_str).is_some()
                    {
                        "verified"
                    } else {
                        "not_checked"
                    }
                });
            let status = match raw_status {
                "resolved" | "verified" => "verified",
                "unresolved" | "unverified" | "transient_unknown" | "malformed" | "conflict"
                | "not_checked" | "retracted" | "not_found" => raw_status,
                _ => "not_checked",
            };
            let evidence = validation
                .get("evidence")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let source = validation
                .get("source")
                .or_else(|| validation.get("verified_via"))
                .and_then(Value::as_str)
                .map(|source| source.to_ascii_lowercase())
                .unwrap_or_else(|| "crossref".to_string());
            let source = match source.as_str() {
                "crossref"
                | "crossref_retraction"
                | "crossref_bibliographic"
                | "doi_resolver"
                | "openalex"
                | "arxiv"
                | "corpus"
                | "semantic_scholar"
                | "ads"
                | "inspire_hep"
                | "zbmath"
                | "gemini_grounded"
                | "citation_waterfall" => source,
                _ => "crossref".to_string(),
            };
            json!({
                "key": key,
                "source": source,
                "status": status,
                "resolved_doi": validation
                    .get("resolved_doi")
                    .cloned()
                    .or_else(|| citation.get("doi").cloned())
                    .unwrap_or(Value::Null),
                "resolved_url": validation
                    .get("resolved_url")
                    .cloned()
                    .unwrap_or(Value::Null),
                "evidence": evidence,
            })
        })
        .collect();

    let similarity_results: Vec<Value> = citations
        .iter()
        .map(|citation| {
            let contexts = citation
                .get("contexts")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            json!({
                "key": citation.get("key").and_then(Value::as_str).unwrap_or("").to_string(),
                "score": if contexts > 0 { 1.0 } else { 0.0 },
                "method": "deterministic_context_presence",
                "notes": if contexts > 0 { Value::Null } else { Value::String("citation has no context".to_string()) },
            })
        })
        .collect();

    let mut graph_warnings = Vec::new();
    if !unmatched.is_empty() {
        graph_warnings.push(json!({
            "code": "unmatched_citation_keys",
            "message": "Citation markers were found without matching bibliography entries.",
            "keys": unmatched,
        }));
    }
    if !uncited.is_empty() {
        graph_warnings.push(json!({
            "code": "uncited_bibliography_keys",
            "message": "Bibliography entries were found without citation contexts.",
            "keys": uncited,
        }));
    }

    let mut remediation_items = Vec::new();
    for warning in &graph_warnings {
        if let Some(keys) = warning.get("keys").and_then(Value::as_array) {
            for key in keys.iter().filter_map(Value::as_str) {
                remediation_items.push(json!({
                    "key": key,
                    "action": "repair_reference_graph",
                    "reason": warning.get("message").and_then(Value::as_str).unwrap_or("citation graph warning"),
                }));
            }
        }
    }

    let resolver_needs_remediation = resolver_results.iter().any(|result| {
        matches!(
            result.get("status").and_then(Value::as_str),
            Some("retracted" | "unresolved" | "malformed" | "conflict" | "not_found")
        )
    });
    for result in &resolver_results {
        if matches!(
            result.get("status").and_then(Value::as_str),
            Some("retracted" | "unresolved" | "malformed" | "conflict" | "not_found")
        ) {
            remediation_items.push(json!({
                "key": result.get("key").cloned().unwrap_or(Value::Null),
                "action": "repair_or_remove_reference",
                "reason": format!(
                    "resolver status {} requires remediation",
                    result.get("status").and_then(Value::as_str).unwrap_or("unknown")
                ),
            }));
        }
    }

    let status = if graph_warnings.is_empty() && !resolver_needs_remediation {
        "verified"
    } else {
        "needs_remediation"
    };
    json!({
        "status": status,
        "summary": format!(
            "{} references parsed; {} graph warnings",
            parsed_references.len(),
            graph_warnings.len()
        ),
        "parsed_references": parsed_references,
        "resolver_results": resolver_results,
        "similarity_results": similarity_results,
        "metadata_conflicts": Vec::<Value>::new(),
        "graph_warnings": graph_warnings,
        "remediation_items": remediation_items,
    })
}

fn apply_macros(_bundle: &mut ArtifactBundle, _res: Option<StageOutcome>) {
    // The MacroExpander returns `{normalized_tex, expansions_applied}`. We
    // don't currently re-route normalized_tex back into Stage 5/6/7 because
    // those stages read from `body.md` and optional `semantic_ast`. The result
    // is recorded in `extraction_report.json` via stage outcome bookkeeping.
}

fn deterministic_macros_outcome(raw_tex: Option<&str>) -> Option<StageOutcome> {
    let started = std::time::Instant::now();
    Some(StageOutcome {
        output: json!({
            "normalized_tex": Value::Null,
            "expansions_applied": [],
            "reason": if raw_tex.is_some_and(raw_tex_has_macro_definition) {
                "macro expansion deferred to deterministic TeX/Pandoc path"
            } else {
                "no_macros_detected_by_local_scan"
            },
        }),
        tool_calls: Vec::new(),
        cost_usd: 0.0,
        latency_ms: started.elapsed().as_millis() as i64,
        iters: 0,
        model: "pandoc-macro-scan".into(),
        runner: "rust_fn".into(),
        provenance: NodeProvenance::rust_tool(
            "macro_scan",
            "scan_macro_definitions",
            ["source.tar.gz"],
            ["extraction_report.json"],
            ExtractionMode::PandocEnabled,
        ),
    })
}

fn should_use_deterministic_fallback(
    name: &str,
    res: Option<&StageOutcome>,
    opts: &IngestOptions,
    semantic_ctx: &SemanticSuccessCtx<'_>,
) -> bool {
    if opts.should_skip(name) {
        return false;
    }
    match res {
        None => true,
        Some(outcome) => semantic_check(name, &outcome.output, semantic_ctx).is_some(),
    }
}

fn deterministic_equations_or_empty(
    arxiv_id: &str,
    semantic_ast: Option<&Value>,
    body_md: &str,
) -> Option<StageOutcome> {
    deterministic_equations_outcome(arxiv_id, semantic_ast, body_md).or_else(|| {
        Some(deterministic_empty_outcome(
            "equations",
            json!({
                "equations": [],
                "reason": "no_equations_detected_by_local_scan",
            }),
        ))
    })
}

fn deterministic_equations_outcome(
    _arxiv_id: &str,
    semantic_ast: Option<&Value>,
    body_md: &str,
) -> Option<StageOutcome> {
    let started = std::time::Instant::now();
    let mut listed = semantic_ast
        .map(grokrxiv_extraction::extraction::equations::tools::list_from_ast)
        .unwrap_or_default();
    if listed.is_empty() {
        listed =
            grokrxiv_extraction::extraction::equations::tools::list_from_markdown_body(body_md);
    }
    let equations: Vec<Value> = listed
        .into_iter()
        .filter_map(|e| {
            let id = e.get("id").and_then(Value::as_str)?.to_string();
            let tex = e.get("tex").and_then(Value::as_str)?.trim().to_string();
            if tex.is_empty() {
                return None;
            }
            let hash = grokrxiv_extraction::extraction::equations::tools::equation_hash(&tex);
            Some(json!({
                "id": id,
                "canonical_tex": tex,
                "mathml": Value::Null,
                "semantic_tag": "other",
                "section_id": Value::Null,
                "hash": hash,
            }))
        })
        .collect();
    if equations.is_empty() {
        return None;
    }
    Some(StageOutcome {
        output: json!({ "equations": equations, "reason": Value::Null }),
        tool_calls: Vec::new(),
        cost_usd: 0.0,
        latency_ms: started.elapsed().as_millis() as i64,
        iters: 0,
        model: "pandoc-equation-scan".into(),
        runner: "rust_fn".into(),
        provenance: NodeProvenance::rust_tool(
            "derive_equations",
            "scan_equations",
            ["body.md", "semantic_ast.json"],
            ["equations.json"],
            ExtractionMode::PandocEnabled,
        ),
    })
}

fn deterministic_theorems_or_empty(_arxiv_id: &str, body_md: &str) -> Option<StageOutcome> {
    deterministic_theorems_outcome(_arxiv_id, body_md).or_else(|| {
        Some(deterministic_empty_outcome(
            "theorems",
            json!({
                "nodes": [],
                "reason": "no_theorems_detected_by_local_scan",
            }),
        ))
    })
}

fn deterministic_theorems_outcome(_arxiv_id: &str, body_md: &str) -> Option<StageOutcome> {
    let started = std::time::Instant::now();
    let sections =
        grokrxiv_extraction::extraction::theorems::tools::sections_from_markdown(body_md);
    let mut nodes: Vec<Value> = Vec::new();
    if sections.is_empty() {
        append_theorem_blocks(&mut nodes, None, body_md);
    } else {
        for section in sections {
            let start = section.char_start.min(body_md.len());
            let end = section.char_end.min(body_md.len()).max(start);
            append_theorem_blocks(&mut nodes, Some(section.id.as_str()), &body_md[start..end]);
        }
    }
    if nodes.is_empty() {
        return None;
    }
    Some(StageOutcome {
        output: json!({ "nodes": nodes, "reason": Value::Null }),
        tool_calls: Vec::new(),
        cost_usd: 0.0,
        latency_ms: started.elapsed().as_millis() as i64,
        iters: 0,
        model: "pandoc-theorem-scan".into(),
        runner: "rust_fn".into(),
        provenance: NodeProvenance::rust_tool(
            "derive_theorems",
            "scan_theorems",
            ["body.md"],
            ["theorem_graph.json"],
            ExtractionMode::PandocEnabled,
        ),
    })
}

fn should_use_citation_fallback(
    res: Option<&StageOutcome>,
    opts: &IngestOptions,
    body_md: &str,
) -> bool {
    if opts.should_skip("citations") {
        return false;
    }
    let has_sites =
        !grokrxiv_extraction::extraction::citations::tools::extract_citation_sites(body_md)
            .is_empty();
    if !has_sites {
        return false;
    }
    match res {
        None => true,
        Some(outcome) => outcome
            .output
            .get("citations")
            .and_then(Value::as_array)
            .map(|a| a.is_empty())
            .unwrap_or(true),
    }
}

fn deterministic_citations_outcome(
    _arxiv_id: &str,
    body_md: &str,
    extract: &PaperExtract,
) -> Option<StageOutcome> {
    use std::collections::{BTreeMap, BTreeSet};

    let started = std::time::Instant::now();
    let bibliography_by_key: BTreeMap<String, &grokrxiv_schemas::Citation> = extract
        .bibliography
        .iter()
        .filter_map(|c| citation_key(c).map(|key| (key, c)))
        .collect();
    let sites = grokrxiv_extraction::extraction::citations::tools::extract_citation_sites(body_md);
    if sites.is_empty() && bibliography_by_key.is_empty() {
        return Some(deterministic_empty_outcome(
            "citations",
            json!({
                "citations": [],
                "unmatched_citation_keys": [],
                "uncited_bibliography_keys": [],
                "reason": "no_citation_sites_detected_by_local_scan",
            }),
        ));
    }

    let mut by_key: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for site in &sites {
        let Some(key) = site.get("key").and_then(Value::as_str) else {
            continue;
        };
        let section = site
            .get("section")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let sentence = site
            .get("sentence")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        by_key.entry(key.to_string()).or_default().push(json!({
            "section": section,
            "sentence": sentence,
            "use": "cited_in_passing",
        }));
    }
    if by_key.is_empty() && !bibliography_by_key.is_empty() {
        let scan_body = body_without_references(body_md);
        for (key, citation) in &bibliography_by_key {
            for context in author_year_contexts(&scan_body, key, &citation.raw) {
                by_key.entry(key.clone()).or_default().push(context);
            }
        }
    }
    let has_sites = !by_key.is_empty();

    let unmatched_citation_keys: Vec<String> = by_key
        .keys()
        .filter(|key| !bibliography_by_key.contains_key(*key))
        .cloned()
        .collect();
    let uncited_bibliography_keys: Vec<String> = bibliography_by_key
        .keys()
        .filter(|key| !by_key.contains_key(*key))
        .cloned()
        .collect();

    let keys: BTreeSet<String> = bibliography_by_key
        .keys()
        .chain(by_key.keys())
        .cloned()
        .collect();
    let citations: Vec<Value> = keys
        .into_iter()
        .map(|key| {
            let bib = bibliography_by_key.get(&key).copied();
            let contexts = by_key.remove(&key).unwrap_or_default();
            let cited = !contexts.is_empty();
            json!({
                "key": key,
                "raw": bib.map(|c| Value::from(c.raw.as_str())).unwrap_or(Value::Null),
                "title": bib.and_then(|c| c.title.as_deref()).map(Value::from).unwrap_or(Value::Null),
                "authors": [],
                "venue": Value::Null,
                "year": Value::Null,
                "doi": bib.and_then(|c| c.doi.as_deref()).map(Value::from).unwrap_or(Value::Null),
                "arxiv_id": bib.and_then(|c| c.arxiv_id.as_deref()).map(Value::from).unwrap_or(Value::Null),
                "resolved_doi": bib.and_then(|c| c.doi.as_deref()).map(Value::from).unwrap_or(Value::Null),
                "resolved_arxiv_id": bib.and_then(|c| c.arxiv_id.as_deref()).map(Value::from).unwrap_or(Value::Null),
                "cited": cited,
                "contexts": contexts,
                "validation": Value::Null,
            })
        })
        .collect();
    if citations.is_empty() {
        return None;
    }

    Some(StageOutcome {
        output: json!({
            "citations": citations,
            "unmatched_citation_keys": unmatched_citation_keys,
            "uncited_bibliography_keys": uncited_bibliography_keys,
            "reason": if !has_sites {
                Value::String("no_citation_sites_detected_by_local_scan".into())
            } else {
                Value::Null
            },
        }),
        tool_calls: Vec::new(),
        cost_usd: 0.0,
        latency_ms: started.elapsed().as_millis() as i64,
        iters: 0,
        model: "pandoc-citation-scan".into(),
        runner: "rust_fn".into(),
        provenance: NodeProvenance::rust_tool(
            "derive_references",
            "scan_references",
            ["body.md", "references.json"],
            ["references.json"],
            ExtractionMode::PandocEnabled,
        ),
    })
}

fn citation_key(c: &grokrxiv_schemas::Citation) -> Option<String> {
    explicit_citation_key_from_raw(&c.raw)
        .filter(|key| !key.is_empty())
        .or_else(|| c.title.as_ref().map(|title| title.trim().to_string()))
        .or_else(|| citation_author_year_key(&c.raw))
        .filter(|key| !key.is_empty())
}

fn explicit_citation_key_from_raw(raw: &str) -> Option<String> {
    let (key, _) = raw.split_once(": ")?;
    let key = key.trim();
    if key.is_empty() || key.len() > 96 {
        return None;
    }
    key.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | '+'))
        .then(|| key.to_string())
}

fn should_replace_low_quality_citations(
    current: Option<&StageOutcome>,
    deterministic: Option<&StageOutcome>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    let Some(deterministic) = deterministic else {
        return false;
    };
    let current_entries = citation_entries(&current.output);
    let deterministic_entries = citation_entries(&deterministic.output);
    let current_contexts = citation_context_count(current_entries);
    let deterministic_contexts = citation_context_count(deterministic_entries);
    if current_contexts == 0 && deterministic_contexts > 0 {
        return true;
    }
    if deterministic_entries.len() <= current_entries.len() {
        return false;
    }
    if deterministic_entries.len() >= current_entries.len().saturating_mul(2) {
        return true;
    }
    current_entries.iter().any(|entry| {
        entry
            .get("raw")
            .and_then(Value::as_str)
            .is_some_and(|raw| raw.chars().count() > 1_200)
    })
}

fn citation_context_count(entries: &[Value]) -> usize {
    entries
        .iter()
        .map(|entry| {
            entry
                .get("contexts")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        })
        .sum()
}

fn citation_entries(output: &Value) -> &[Value] {
    output
        .get("citations")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn citation_author_year_key(raw: &str) -> Option<String> {
    let (surnames, year) = citation_author_year_parts(raw)?;
    let first = surnames.first()?;
    Some(if surnames.len() >= 3 {
        format!("{first} et al. {year}")
    } else if surnames.len() == 2 {
        format!("{} and {} {year}", surnames[0], surnames[1])
    } else {
        format!("{first} {year}")
    })
}

fn citation_author_year_parts(raw: &str) -> Option<(Vec<String>, String)> {
    let year_re = regex::Regex::new(r"\b(?:19|20)\d{2}[a-z]?\b").ok()?;
    let year_match = year_re.find(raw)?;
    let year = year_match.as_str().to_string();
    let prefix = raw[..year_match.start()].trim();
    let surnames = citation_surnames(prefix);
    if surnames.is_empty() {
        None
    } else {
        Some((surnames, year))
    }
}

fn citation_surnames(prefix: &str) -> Vec<String> {
    let normalized = prefix
        .replace('\n', " ")
        .replace(" and ", "; ")
        .replace("; and ", "; ");
    normalized
        .split(';')
        .filter_map(|part| {
            let part = part
                .trim()
                .trim_start_matches("and ")
                .trim_matches(|c: char| c.is_whitespace() || c == '.');
            if part.is_empty() {
                return None;
            }
            let surname = part
                .split(',')
                .next()
                .unwrap_or(part)
                .split_whitespace()
                .next()
                .unwrap_or(part)
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'');
            let alpha = surname.chars().filter(|c| c.is_alphabetic()).count();
            (alpha >= 2).then(|| surname.to_string())
        })
        .collect()
}

fn body_without_references(body_md: &str) -> String {
    let refs_re = regex::Regex::new(r"(?im)^#{1,6}\s+(references|bibliography)\s*$").unwrap();
    refs_re
        .find(body_md)
        .map(|m| body_md[..m.start()].to_string())
        .unwrap_or_else(|| body_md.to_string())
}

fn author_year_contexts(body_md: &str, key: &str, raw: &str) -> Vec<Value> {
    let Some((surnames, year)) = citation_author_year_parts(raw) else {
        return Vec::new();
    };
    let labels = citation_author_year_labels(&surnames, &year);
    let sections = citation_section_index(body_md);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for label in labels {
        let pattern = flexible_author_year_pattern(&label);
        let Ok(re) =
            regex::RegexBuilder::new(&format!(r"(?i)(^|[^[:alnum:]])({pattern})([^[:alnum:]]|$)"))
                .multi_line(true)
                .build()
        else {
            continue;
        };
        for caps in re.captures_iter(body_md) {
            let Some(m) = caps.get(2) else {
                continue;
            };
            let sentence = citation_surrounding_sentence(body_md, m.start());
            let section = citation_section_for_offset(m.start(), &sections);
            if seen.insert((section.clone(), sentence.clone())) {
                out.push(json!({
                    "section": section,
                    "sentence": sentence,
                    "use": "cited_in_passing",
                    "matched": label,
                    "key": key,
                }));
            }
        }
    }
    out
}

fn citation_author_year_labels(surnames: &[String], year: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let Some(first) = surnames.first() else {
        return labels;
    };
    labels.push(format!("{first} {year}"));
    if surnames.len() == 2 {
        labels.push(format!("{} and {} {year}", surnames[0], surnames[1]));
    }
    if surnames.len() >= 3 {
        labels.push(format!("{first} et al. {year}"));
        labels.push(format!(
            "{}, {}, and {} {year}",
            surnames[0], surnames[1], surnames[2]
        ));
    }
    labels.sort();
    labels.dedup();
    labels
}

fn flexible_author_year_pattern(label: &str) -> String {
    regex::escape(label).replace(r"\ ", r"\s+")
}

fn citation_section_index(body: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                out.push((heading.to_string(), offset));
            }
        }
        offset += line.len();
    }
    out
}

fn citation_section_for_offset(offset: usize, sections: &[(String, usize)]) -> String {
    sections
        .iter()
        .take_while(|(_, start)| *start <= offset)
        .last()
        .map(|(heading, _)| heading.clone())
        .unwrap_or_default()
}

fn citation_surrounding_sentence(body: &str, offset: usize) -> String {
    let bytes = body.as_bytes();
    let mut start = 0usize;
    let mut idx = offset.min(bytes.len());
    while idx > 0 {
        idx -= 1;
        if matches!(bytes[idx], b'.' | b'?' | b'!' | b'\n') {
            start = idx + 1;
            break;
        }
    }
    let mut end = bytes.len();
    idx = offset.min(bytes.len());
    while idx < bytes.len() {
        if matches!(bytes[idx], b'.' | b'?' | b'!' | b'\n') {
            end = idx + 1;
            break;
        }
        idx += 1;
    }
    body[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn deterministic_empty_outcome(stage: &str, output: Value) -> StageOutcome {
    StageOutcome {
        output,
        tool_calls: Vec::new(),
        cost_usd: 0.0,
        latency_ms: 0,
        iters: 0,
        model: format!("pandoc-{stage}-scan"),
        runner: "rust_fn".into(),
        provenance: pandoc_empty_provenance(stage),
    }
}

fn pandoc_empty_provenance(stage: &str) -> NodeProvenance {
    match stage {
        "equations" => NodeProvenance::rust_tool(
            "derive_equations",
            "scan_equations",
            ["body.md", "semantic_ast.json"],
            ["equations.json"],
            ExtractionMode::PandocEnabled,
        ),
        "theorems" => NodeProvenance::rust_tool(
            "derive_theorems",
            "scan_theorems",
            ["body.md"],
            ["theorem_graph.json"],
            ExtractionMode::PandocEnabled,
        ),
        "citations" => NodeProvenance::rust_tool(
            "derive_references",
            "scan_references",
            ["body.md", "references.json"],
            ["references.json"],
            ExtractionMode::PandocEnabled,
        ),
        _ => NodeProvenance::rust_tool(
            format!("{stage}_scan"),
            format!("scan_{stage}"),
            ["body.md"],
            ["extraction_report.json"],
            ExtractionMode::PandocEnabled,
        ),
    }
}

fn build_artifact_provenance(
    source_body_producer: SourceBodyProducer,
    mode: ExtractionMode,
    equations: Option<&StageOutcome>,
    theorems: Option<&StageOutcome>,
    citations: Option<&StageOutcome>,
) -> Vec<ArtifactProvenance> {
    let body_node = source_body_producer.provenance(mode);
    let metadata_node = NodeProvenance::rust_tool(
        "derive_metadata",
        "build_metadata_json",
        ["arxiv_metadata", "paper_extract"],
        ["metadata.json"],
        mode,
    );
    let sections_node = if source_body_producer == SourceBodyProducer::VlmAgent {
        body_node.clone()
    } else {
        NodeProvenance::rust_tool(
            "derive_sections",
            "build_sections_json",
            ["body.md", "paper_extract"],
            ["sections.json"],
            mode,
        )
    };
    let equations_node = equations
        .map(|outcome| outcome.provenance.clone())
        .unwrap_or_else(|| {
            NodeProvenance::rust_tool(
                "derive_equations",
                "scan_equations",
                ["body.md", "semantic_ast.json"],
                ["equations.json"],
                ExtractionMode::PandocEnabled,
            )
        });
    let references_node = citations
        .map(|outcome| outcome.provenance.clone())
        .unwrap_or_else(|| {
            NodeProvenance::rust_tool(
                "derive_references",
                "build_initial_references_json",
                ["paper_extract.bibliography"],
                ["references.json"],
                mode,
            )
        });
    let theorem_node = theorems
        .map(|outcome| outcome.provenance.clone())
        .unwrap_or_else(|| {
            NodeProvenance::rust_tool(
                "derive_theorems",
                "scan_theorems",
                ["body.md"],
                ["theorem_graph.json"],
                ExtractionMode::PandocEnabled,
            )
        });
    let extraction_report_node = NodeProvenance::artifact(
        "materialize_extraction_report",
        "build_extraction_report",
        [
            "stage_reports",
            "artifact_provenance",
            "tool_call_log.jsonl",
        ],
        ["extraction_report.json"],
        mode,
    );
    let review_input_node = NodeProvenance::artifact(
        "materialize_review_input",
        "build_review_input",
        [
            "metadata.json",
            "body.md",
            "sections.json",
            "equations.json",
            "references.json",
            "theorem_graph.json",
            "extraction_report.json",
        ],
        ["review_input.json"],
        mode,
    );

    vec![
        ArtifactProvenance::from_node("body.md", &body_node),
        ArtifactProvenance::from_node("metadata.json", &metadata_node),
        ArtifactProvenance::from_node("sections.json", &sections_node),
        ArtifactProvenance::from_node("equations.json", &equations_node),
        ArtifactProvenance::from_node("references.json", &references_node),
        ArtifactProvenance::from_node("theorem_graph.json", &theorem_node),
        ArtifactProvenance::from_node("extraction_report.json", &extraction_report_node),
        ArtifactProvenance::from_node("review_input.json", &review_input_node),
    ]
}

fn append_theorem_blocks(nodes: &mut Vec<Value>, section_id: Option<&str>, body: &str) {
    for block in grokrxiv_extraction::extraction::theorems::tools::scan_theorem_blocks(body) {
        let node_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("theorem")
            .to_string();
        let statement = block
            .get("statement_preview")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if statement.is_empty() {
            continue;
        }
        let id = block
            .get("label")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("thm-{}", nodes.len() + 1));
        nodes.push(json!({
            "id": id,
            "type": node_type,
            "statement": statement,
            "section_id": section_id.map(Value::from).unwrap_or(Value::Null),
            "depends_on": [],
        }));
    }
}

// ---------------------------------------------------------------------------
// extraction_report.json bookkeeping
// ---------------------------------------------------------------------------

struct StageReport {
    name: String,
    status: String,
    duration_ms: Option<i64>,
    cost_usd: Option<f64>,
    model: Option<String>,
    runner: Option<String>,
    warnings: Vec<String>,
    iters: Option<u32>,
    tool_call_summary: Option<Value>,
    provenance: Option<NodeProvenance>,
}

impl StageReport {
    fn ok(
        name: &str,
        duration_ms: Option<i64>,
        cost_usd: Option<f64>,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: "ok".into(),
            duration_ms,
            cost_usd,
            model: None,
            runner: None,
            warnings,
            iters: None,
            tool_call_summary: None,
            provenance: None,
        }
    }
    fn degraded(name: &str, warning: &str) -> Self {
        Self {
            name: name.into(),
            status: "degraded".into(),
            duration_ms: None,
            cost_usd: None,
            model: None,
            runner: None,
            warnings: vec![warning.into()],
            iters: None,
            tool_call_summary: None,
            provenance: None,
        }
    }
    fn failed(name: &str, warning: &str) -> Self {
        Self {
            name: name.into(),
            status: "failed".into(),
            duration_ms: None,
            cost_usd: None,
            model: None,
            runner: None,
            warnings: vec![warning.into()],
            iters: None,
            tool_call_summary: None,
            provenance: None,
        }
    }
    fn skipped(name: &str, reason: &str) -> Self {
        Self {
            name: name.into(),
            status: "skipped".into(),
            duration_ms: None,
            cost_usd: None,
            model: None,
            runner: None,
            warnings: vec![reason.into()],
            iters: None,
            tool_call_summary: None,
            provenance: None,
        }
    }
    fn with_provenance(mut self, provenance: NodeProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
    fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "status": self.status,
            "duration_ms": self.duration_ms,
            "cost_usd": self.cost_usd,
            "model": self.model,
            "runner": self.runner,
            "warnings": self.warnings,
            "iters": self.iters,
            "tool_call_summary": self.tool_call_summary.clone().unwrap_or(Value::Null),
            "provenance": self.provenance.as_ref().map(NodeProvenance::to_value).unwrap_or(Value::Null),
            "executor": self.provenance.as_ref().map(|p| p.executor.as_str()),
            "tool": self.provenance.as_ref().and_then(|p| p.tool.as_deref()),
            "agent": self.provenance.as_ref().and_then(|p| p.agent.as_deref()),
            "mode": self.provenance.as_ref().map(|p| p.mode.as_str()),
            "inputs": self.provenance.as_ref().map(|p| p.inputs.clone()).unwrap_or_default(),
            "outputs": self.provenance.as_ref().map(|p| p.outputs.clone()).unwrap_or_default(),
        })
    }
}

fn source_to_body_stage_report(
    producer: SourceBodyProducer,
    extraction_mode: ExtractionMode,
    body_md: &str,
) -> StageReport {
    let provenance = producer.provenance(extraction_mode);
    if body_md.trim().is_empty() {
        StageReport::failed("source_to_body", "source-to-body produced empty body.md")
            .with_provenance(provenance)
    } else {
        StageReport::ok("source_to_body", None, None, Vec::new()).with_provenance(provenance)
    }
}

fn record_agent_outcome(
    out: &mut Vec<StageReport>,
    name: &str,
    res: Option<&StageOutcome>,
    opts: &IngestOptions,
    semantic_ctx: &SemanticSuccessCtx<'_>,
) {
    if opts.should_skip(name) {
        out.push(StageReport::skipped(name, "skipped via --skip-stages"));
        return;
    }
    match res {
        Some(outcome) => {
            let summary = tool_call_summary(&outcome.tool_calls);
            let semantic = semantic_check(name, &outcome.output, semantic_ctx);
            let mut report = StageReport::ok(
                name,
                Some(outcome.latency_ms),
                Some(outcome.cost_usd as f64),
                Vec::new(),
            );
            report.model = Some(outcome.model.clone());
            report.runner = Some(outcome.runner.clone());
            report.iters = Some(outcome.iters);
            report.tool_call_summary = Some(summary);
            report.provenance = Some(outcome.provenance.clone());
            if let Some(warning) = semantic {
                report.status = "degraded".into();
                report.warnings.push(warning);
            }
            out.push(report);
        }
        None => {
            out.push(StageReport::degraded(name, "agent returned no output"));
        }
    }
}

/// Context the semantic-success check needs to decide whether an "empty"
/// submission is genuine or a tool failure.
pub(crate) struct SemanticSuccessCtx<'a> {
    pub extract: &'a PaperExtract,
    /// Concatenated TeX source from the unpacked tarball when present;
    /// used by the macros rule to detect any `\newcommand` etc.
    pub raw_tex: Option<&'a str>,
}

/// Returns `Some(warning_message)` if the stage's output looks empty when
/// the paper clearly has content of the relevant kind. Agents can override
/// by adding the optional top-level `reason: "no_<thing>_in_paper"` field
/// — when set to a non-null value we accept the empty submission and skip
/// the warning.
fn semantic_check(name: &str, output: &Value, ctx: &SemanticSuccessCtx<'_>) -> Option<String> {
    // Honour the agent's explicit override.
    let reason = output.get("reason").and_then(Value::as_str);
    if reason.is_some_and(|r| !r.is_empty()) {
        return None;
    }

    match name {
        "equations" => {
            let n = output
                .get("equations")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            let sections = ctx.extract.sections.len();
            if n == 0 && sections > 0 {
                Some(format!(
                    "agent submitted {{equations:[]}} but paper has {sections} sections — likely tool failure"
                ))
            } else {
                None
            }
        }
        "theorems" => {
            let n = output
                .get("theorem_graph")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or_else(|| {
                    output
                        .get("nodes")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0)
                });
            if n == 0 && has_theorem_like_heading(ctx.extract) {
                Some(
                    "agent submitted empty theorem_graph but paper has theorem/lemma/proposition/corollary/proof/definition headings — likely tool failure".to_string(),
                )
            } else {
                None
            }
        }
        "citations" => {
            let n = output
                .get("citations")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            let bib = ctx.extract.bibliography.len();
            if n == 0 && bib > 0 {
                Some(format!(
                    "agent submitted {{citations:[]}} but paper has {bib} bibliography entries — likely tool failure"
                ))
            } else {
                None
            }
        }
        "macros" => {
            let n = output
                .get("expansions_applied")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            if n == 0 && ctx.raw_tex.is_some_and(raw_tex_has_macro_definition) {
                Some(
                    "agent submitted empty expansions_applied but raw TeX contains \\newcommand/\\def/\\renewcommand/\\DeclareMathOperator — likely tool failure".to_string(),
                )
            } else {
                None
            }
        }
        "vlm" => {
            let title = output.get("title").and_then(Value::as_str).unwrap_or("");
            let abstract_ = output.get("abstract").and_then(Value::as_str).unwrap_or("");
            if title.is_empty() || abstract_.is_empty() {
                Some("VLM extractor produced an empty title or abstract".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn has_theorem_like_heading(extract: &PaperExtract) -> bool {
    // Cheap word-level scan; the patterns are anchored to common LaTeX/markdown
    // styles so we don't need a regex compile per check.
    let needles = [
        "theorem",
        "lemma",
        "proposition",
        "corollary",
        "proof",
        "definition",
    ];
    for s in &extract.sections {
        let h = s.heading.to_lowercase();
        for n in &needles {
            if h.contains(n) {
                return true;
            }
        }
    }
    false
}

fn raw_tex_has_macro_definition(tex: &str) -> bool {
    tex.contains("\\newcommand")
        || tex.contains("\\def")
        || tex.contains("\\renewcommand")
        || tex.contains("\\DeclareMathOperator")
}

/// Build a compact `{count, by_tool}` summary of a stage's tool-call audit
/// log. The full per-call log lives in `bundle.tool_call_log` (uploaded to
/// `review-artifacts/<arxiv_id>/tool_call_log.jsonl`); the summary is what
/// surfaces in `extraction_report.json` for Tier-1 readers.
fn tool_call_summary(calls: &[ToolCallRecord]) -> Value {
    use std::collections::BTreeMap;
    let mut by_tool: BTreeMap<&str, u32> = BTreeMap::new();
    for c in calls {
        *by_tool.entry(c.tool.as_str()).or_insert(0) += 1;
    }
    let by_tool_value: serde_json::Map<String, Value> = by_tool
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::from(v)))
        .collect();
    json!({
        "count": calls.len(),
        "by_tool": by_tool_value,
    })
}

/// Append every per-call audit record from one stage's outcome into the
/// run-wide audit log (one JSON object per entry, tagged with `stage_name`
/// and a per-stage ordinal).
fn push_tool_calls(out: &mut Vec<Value>, stage_name: &str, outcome: Option<&StageOutcome>) {
    let Some(outcome) = outcome else {
        return;
    };
    for (ordinal, call) in outcome.tool_calls.iter().enumerate() {
        out.push(json!({
            "stage_name": stage_name,
            "ordinal": ordinal,
            "call_record": call,
        }));
    }
}

// ---------------------------------------------------------------------------
// Cached short-circuit
// ---------------------------------------------------------------------------

async fn load_cached(
    paper_id: Uuid,
    git_path: &str,
    storage_prefix: Option<&str>,
) -> Result<IngestResult> {
    let repo_root: PathBuf = std::env::var("GROKRXIV_DATA_REPO_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/mlong/Documents/Development/grokrxiv-data"));
    let review_input = load_review_input_from_disk(&repo_root, git_path)?;
    let extract = load_paper_extract(&repo_root, &review_input)?;
    let pointer = PersistedPointer {
        paper_id: paper_id.to_string(),
        arxiv_id: review_input.arxiv_id.clone(),
        git_path: git_path.to_string(),
        git_commit_sha: None,
        storage_prefix: storage_prefix.unwrap_or(&review_input.arxiv_id).to_string(),
        extraction_cost_usd: None,
        routed: Vec::new(),
    };
    Ok(IngestResult {
        paper_id,
        pointer,
        review_input,
        extract,
    })
}

fn load_review_input_from_disk(repo_root: &Path, git_path: &str) -> Result<ReviewInput> {
    let path = repo_root.join(git_path).join("review_input.json");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read cached review_input.json from {}", path.display()))?;
    let parsed: ReviewInput = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse cached review_input.json from {}", path.display()))?;
    Ok(parsed)
}

/// Reconstruct a [`PaperExtract`] from a cached `review_input.json` + the
/// Tier-1 files it points at. Used both by `load_cached` (idempotent path)
/// and by the supervisor's review entry point when reading from a previously
/// extracted paper.
///
/// The `body_markdown` field of `review_input.json` is either a relative
/// Tier-1 path (e.g. `papers/<id>/body.md`) or a `supabase://` URI. We
/// resolve the Tier-1 path locally; the URI form is left for a follow-up
/// (the review path only needs `body_markdown` today, not the raw bytes from
/// Tier 2).
pub fn load_paper_extract(repo_root: &Path, ri: &ReviewInput) -> Result<PaperExtract> {
    let body = if ri.body_markdown.starts_with("supabase://") {
        // Tier 2 — we don't fetch from here in-band. Return empty body so the
        // review path can at least see the sections.json metadata.
        String::new()
    } else {
        std::fs::read_to_string(repo_root.join(&ri.body_markdown))
            .with_context(|| format!("read body.md from {}", ri.body_markdown))?
    };

    let metadata: Value = serde_json::from_slice(
        &std::fs::read(repo_root.join(&ri.metadata))
            .with_context(|| format!("read metadata.json from {}", ri.metadata))?,
    )?;
    let sections_doc: Value = serde_json::from_slice(
        &std::fs::read(repo_root.join(&ri.sections))
            .with_context(|| format!("read sections.json from {}", ri.sections))?,
    )?;
    let references_doc: Value = std::fs::read(repo_root.join(&ri.references))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({"citations": []}));

    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let abstract_text = metadata
        .get("abstract")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let field = metadata
        .get("primary_category")
        .and_then(Value::as_str)
        .map(str::to_string);
    let authors: Vec<grokrxiv_schemas::Author> = metadata
        .get("authors")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some(grokrxiv_schemas::Author {
                        name: a.get("name")?.as_str()?.to_string(),
                        affiliation: a
                            .get("affiliation")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        email: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let sections: Vec<grokrxiv_schemas::Section> = sections_doc
        .get("sections")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let heading = s.get("heading")?.as_str()?.to_string();
                    let start = s.get("char_start")?.as_i64().unwrap_or(0) as usize;
                    let end = s.get("char_end")?.as_i64().unwrap_or(0) as usize;
                    let body_md = body
                        .get(start..end.min(body.len()))
                        .unwrap_or("")
                        .to_string();
                    Some(grokrxiv_schemas::Section {
                        heading,
                        body_markdown: body_md,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let bibliography: Vec<grokrxiv_schemas::Citation> = references_doc
        .get("citations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| grokrxiv_schemas::Citation {
                    raw: c
                        .get("raw")
                        .or_else(|| c.get("key"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    doi: c.get("doi").and_then(Value::as_str).map(str::to_string),
                    arxiv_id: c
                        .get("arxiv_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    title: c.get("title").and_then(Value::as_str).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PaperExtract {
        arxiv_id: ri.arxiv_id.clone(),
        title,
        authors,
        abstract_: abstract_text,
        field,
        sections,
        figures: Vec::new(),
        bibliography,
        source_format: None,
    })
}

// ---------------------------------------------------------------------------
// Unit tests for FP-RPT3a A4 semantic-success rules
// ---------------------------------------------------------------------------
#[cfg(test)]
mod a4_tests {
    use super::*;
    use grokrxiv_schemas::{Citation, PaperExtract, Section};

    fn extract_with(sections: Vec<&str>, bib: usize) -> PaperExtract {
        PaperExtract {
            arxiv_id: "test".into(),
            title: "t".into(),
            authors: vec![],
            abstract_: "a".into(),
            field: None,
            sections: sections
                .into_iter()
                .map(|h| Section {
                    heading: h.into(),
                    body_markdown: "body".into(),
                })
                .collect(),
            figures: vec![],
            bibliography: (0..bib)
                .map(|i| Citation {
                    raw: format!("ref{i}"),
                    doi: None,
                    arxiv_id: None,
                    title: None,
                })
                .collect(),
            source_format: None,
        }
    }

    fn test_extraction_routing(stage: &str) -> ExtractionRouting {
        let agents_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../agents");
        let path = agents_dir.join("extraction").join(format!("{stage}.yaml"));
        let yaml = std::fs::read_to_string(&path).expect("read extraction routing fixture");
        serde_yaml::from_str(&yaml).expect("parse extraction routing fixture")
    }

    #[test]
    fn source_to_body_report_marks_empty_body_failed() {
        let report = source_to_body_stage_report(
            SourceBodyProducer::PandocTex,
            ExtractionMode::PandocEnabled,
            "",
        );
        let value = report.to_value();

        assert_eq!(value["name"], "source_to_body");
        assert_eq!(value["status"], "failed");
        assert_eq!(value["tool"], "pandoc_tex_to_markdown");
        assert!(value["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|msg| msg.contains("empty body.md"))));
    }

    #[test]
    fn source_to_body_report_names_raw_tex_fallback() {
        let report = source_to_body_stage_report(
            SourceBodyProducer::RawTexFallback,
            ExtractionMode::PandocEnabled,
            "## Main\n\nRecovered body.",
        );
        let value = report.to_value();

        assert_eq!(value["name"], "source_to_body");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["tool"], "raw_tex_markdown_fallback");
        assert_eq!(value["executor"], "tool");
    }

    #[test]
    fn equations_empty_with_sections_is_degraded() {
        let extract = extract_with(vec!["Intro", "Methods", "Results"], 0);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "equations": [], "reason": null });
        let w = semantic_check("equations", &out, &ctx);
        assert!(w.is_some(), "should warn when 3 sections but 0 equations");
        assert!(w.unwrap().contains("3 sections"));
    }

    #[test]
    fn equations_reason_overrides_warning() {
        let extract = extract_with(vec!["Intro"], 0);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "equations": [], "reason": "no_equations_in_paper" });
        assert!(semantic_check("equations", &out, &ctx).is_none());
    }

    #[test]
    fn theorems_warns_when_theorem_heading_present() {
        let extract = extract_with(vec!["Introduction", "Main Theorem", "Proof"], 0);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "theorem_graph": [], "reason": null });
        assert!(semantic_check("theorems", &out, &ctx).is_some());
    }

    #[test]
    fn theorems_no_warning_when_no_theorem_headings() {
        let extract = extract_with(vec!["Intro", "Discussion"], 0);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "theorem_graph": [], "reason": null });
        assert!(semantic_check("theorems", &out, &ctx).is_none());
    }

    #[test]
    fn citations_warns_when_bibliography_nonempty() {
        let extract = extract_with(vec!["Intro"], 5);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "citations": [], "reason": null });
        let w = semantic_check("citations", &out, &ctx);
        assert!(w.is_some());
        assert!(w.unwrap().contains("5 bibliography entries"));
    }

    #[test]
    fn macros_warns_when_raw_tex_has_newcommand() {
        let extract = extract_with(vec!["Intro"], 0);
        let tex = "\\newcommand{\\R}{\\mathbb{R}}\n";
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: Some(tex),
        };
        let out = json!({ "normalized_tex": "x", "expansions_applied": [], "reason": null });
        assert!(semantic_check("macros", &out, &ctx).is_some());
    }

    #[test]
    fn macros_quiet_when_raw_tex_has_no_definitions() {
        let extract = extract_with(vec!["Intro"], 0);
        let tex = "Plain TeX with no definitions.\n";
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: Some(tex),
        };
        let out = json!({ "normalized_tex": "x", "expansions_applied": [], "reason": null });
        assert!(semantic_check("macros", &out, &ctx).is_none());
    }

    #[test]
    fn vlm_warns_when_title_or_abstract_empty() {
        let extract = extract_with(vec!["Intro"], 0);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let out = json!({ "title": "", "abstract": "x" });
        assert!(semantic_check("vlm", &out, &ctx).is_some());
        let out2 = json!({ "title": "x", "abstract": "" });
        assert!(semantic_check("vlm", &out2, &ctx).is_some());
        let out3 = json!({ "title": "x", "abstract": "y" });
        assert!(semantic_check("vlm", &out3, &ctx).is_none());
    }

    /// FP-RPT3a A5: `extraction_budget_for` honours the YAML number, not the
    /// hardcoded inline ceiling. `agents/extraction/macros.yaml` says
    /// `max_cost_usd: 0.40` and `max_iters: 20`, so the resolved budget MUST
    /// match — regardless of where in the YAML structure the values live
    /// (macros uses `loop:` block; the rest top-level).
    #[test]
    fn macros_budget_resolves_to_yaml_values() {
        let routing = test_extraction_routing("macros");
        let b = extraction_budget_from_routing("macros", Some(&routing));
        // YAML uses `loop: {max_cost_usd: 0.40, max_iters: 20}` — both should
        // surface through the resolved getter, not the hardcoded fallback.
        assert!(
            (b.max_cost_usd - 0.40).abs() < 1e-6,
            "macros budget must equal $0.40 (got ${})",
            b.max_cost_usd
        );
        assert_eq!(b.max_iters, 20);
    }

    /// Same idea for citations.yaml (top-level form, not nested under `loop:`).
    #[test]
    fn citations_budget_resolves_to_yaml_values() {
        let routing = test_extraction_routing("citations");
        let b = extraction_budget_from_routing("citations", Some(&routing));
        assert!(
            (b.max_cost_usd - 0.50).abs() < 1e-6,
            "citations budget must equal $0.50 (got ${})",
            b.max_cost_usd
        );
        assert_eq!(b.max_iters, 80);
    }

    #[test]
    fn extractor_resolves_to_yaml_cli_by_default() {
        let routing = test_extraction_routing("macros");
        assert_eq!(
            resolve_extractor_from_routing("macros", None, None, Some(&routing)),
            ExtractorKind::Cli
        );
    }

    #[test]
    fn extractor_env_overrides_yaml() {
        let routing = test_extraction_routing("macros");
        assert_eq!(
            resolve_extractor_from_routing("macros", Some("api"), None, Some(&routing)),
            ExtractorKind::Api
        );
    }

    #[test]
    fn record_agent_outcome_marks_status_degraded_with_warning() {
        let extract = extract_with(vec!["Intro", "Methods"], 7);
        let ctx = SemanticSuccessCtx {
            extract: &extract,
            raw_tex: None,
        };
        let outcome = StageOutcome {
            output: json!({ "citations": [], "reason": null }),
            tool_calls: vec![],
            cost_usd: 0.0,
            latency_ms: 0,
            iters: 1,
            model: "test-model".into(),
            runner: "api".into(),
            provenance: NodeProvenance::agent(
                "citations",
                "citation_contextualizer",
                ["body.md", "references.json"],
                ["references.json"],
                ExtractionMode::AgentEnabled,
            ),
        };
        let mut reports: Vec<StageReport> = Vec::new();
        let opts = IngestOptions::default();
        record_agent_outcome(&mut reports, "citations", Some(&outcome), &opts, &ctx);
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(r.status, "degraded");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("7 bibliography entries")),
            "warnings: {:?}",
            r.warnings
        );
        // metadata still surfaces on a degraded stage.
        assert_eq!(r.model.as_deref(), Some("test-model"));
        assert_eq!(r.runner.as_deref(), Some("api"));
        assert_eq!(r.iters, Some(1));
    }

    #[test]
    fn extraction_mode_defaults_to_pandoc_enabled_and_rejects_legacy_force_flag() {
        let prev_mode = std::env::var("GROKRXIV_EXTRACTION_MODE").ok();
        let prev_force = std::env::var("GROKRXIV_FORCE_AGENT_EXTRACTION").ok();
        std::env::remove_var("GROKRXIV_EXTRACTION_MODE");
        std::env::remove_var("GROKRXIV_FORCE_AGENT_EXTRACTION");

        assert_eq!(
            extraction_mode_from_env().expect("default extraction mode"),
            ExtractionMode::PandocEnabled
        );

        std::env::set_var("GROKRXIV_EXTRACTION_MODE", "agent_enabled");
        assert_eq!(
            extraction_mode_from_env().expect("agent extraction mode"),
            ExtractionMode::AgentEnabled
        );

        std::env::set_var("GROKRXIV_FORCE_AGENT_EXTRACTION", "1");
        let err = extraction_mode_from_env().expect_err("legacy flag must fail fast");
        assert!(err
            .to_string()
            .contains("GROKRXIV_EXTRACTION_MODE=agent_enabled"));

        match prev_mode {
            Some(v) => std::env::set_var("GROKRXIV_EXTRACTION_MODE", v),
            None => std::env::remove_var("GROKRXIV_EXTRACTION_MODE"),
        }
        match prev_force {
            Some(v) => std::env::set_var("GROKRXIV_FORCE_AGENT_EXTRACTION", v),
            None => std::env::remove_var("GROKRXIV_FORCE_AGENT_EXTRACTION"),
        }
    }

    #[test]
    fn citation_validation_report_flags_graph_remediation_items() {
        let references = json!({
            "citations": [{
                "key": "smith2026",
                "raw": "@article{smith2026,title={A Paper}}",
                "title": "A Paper",
                "authors": ["A. Smith"],
                "venue": null,
                "year": 2026,
                "doi": null,
                "arxiv_id": null,
                "cited": false,
                "contexts": []
            }],
            "unmatched_citation_keys": ["missing2026"],
            "uncited_bibliography_keys": ["smith2026"]
        });

        let report = build_citation_validation_report(&references);
        let schema: Value = serde_json::from_str(include_str!(
            "../../../schemas/citation_validation_report.schema.json"
        ))
        .expect("citation validation report schema");
        let validator = jsonschema::validator_for(&schema).expect("compile schema");

        assert_eq!(report["status"], "needs_remediation");
        assert!(validator.validate(&report).is_ok());
        assert_eq!(report["parsed_references"][0]["key"], "smith2026");
        assert!(report["graph_warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "unmatched_citation_keys"));
        assert!(report["remediation_items"]
            .as_array()
            .expect("remediations")
            .iter()
            .any(|item| item["key"] == "missing2026"));
    }

    #[test]
    fn citation_validation_report_preserves_waterfall_resolver_sources() {
        let references = json!({
            "citations": [
                {
                    "key": "cartan1922",
                    "raw": "@article{cartan1922,title={Relativitaetstheorie und Mathematik}}",
                    "title": "Relativitaetstheorie und Mathematik",
                    "authors": ["E. Cartan"],
                    "venue": null,
                    "year": 1922,
                    "doi": null,
                    "arxiv_id": null,
                    "cited": true,
                    "contexts": [{"section": "Related Work"}],
                    "validation": {
                        "source": "ads",
                        "status": "verified",
                        "resolved_doi": "10.ads/cartan",
                        "resolved_url": "https://ui.adsabs.harvard.edu/abs/1922JRAM..146...16C/abstract",
                        "evidence": ["ADS bibcode 1922JRAM..146...16C"]
                    }
                },
                {
                    "key": "trautman1966",
                    "raw": "@article{trautman1966,title={Foundations and Current Problems of General Relativity}}",
                    "title": "Foundations and Current Problems of General Relativity",
                    "authors": ["A. Trautman"],
                    "venue": null,
                    "year": 1966,
                    "doi": null,
                    "arxiv_id": null,
                    "cited": true,
                    "contexts": [{"section": "Related Work"}],
                    "validation": {
                        "source": "zbmath",
                        "status": "verified",
                        "resolved_doi": "10.zbmath/trautman",
                        "resolved_url": "https://zbmath.org/trautman",
                        "evidence": ["zbMATH Open match"]
                    }
                }
            ],
            "unmatched_citation_keys": [],
            "uncited_bibliography_keys": []
        });

        let report = build_citation_validation_report(&references);
        let schema: Value = serde_json::from_str(include_str!(
            "../../../schemas/citation_validation_report.schema.json"
        ))
        .expect("citation validation report schema");
        let validator = jsonschema::validator_for(&schema).expect("compile schema");

        assert!(validator.validate(&report).is_ok(), "{report:#}");
        assert_eq!(report["status"], "verified");
        assert_eq!(report["resolver_results"][0]["source"], "ads");
        assert_eq!(
            report["resolver_results"][0]["resolved_doi"],
            "10.ads/cartan"
        );
        assert_eq!(report["resolver_results"][1]["source"], "zbmath");
        assert_eq!(
            report["resolver_results"][1]["resolved_url"],
            "https://zbmath.org/trautman"
        );
    }

    #[test]
    fn citation_validation_report_preserves_retraction_evidence() {
        let references = json!({
            "citations": [{
                "key": "majorana2018",
                "raw": "@article{majorana2018,title={Quantized Majorana conductance}}",
                "title": "Quantized Majorana conductance",
                "authors": ["H. Zhang"],
                "venue": "Nature",
                "year": 2018,
                "doi": "10.retracted/majorana",
                "arxiv_id": null,
                "cited": true,
                "contexts": [{"section": "Evidence"}],
                "validation": {
                    "source": "crossref_retraction",
                    "status": "retracted",
                    "resolved_doi": "10.retracted/majorana",
                    "resolved_url": "https://doi.org/10.retracted/majorana",
                    "evidence": [
                        "update-to type=retraction doi=10.notice/retraction source=retraction-watch"
                    ]
                }
            }],
            "unmatched_citation_keys": [],
            "uncited_bibliography_keys": []
        });

        let report = build_citation_validation_report(&references);
        let schema: Value = serde_json::from_str(include_str!(
            "../../../schemas/citation_validation_report.schema.json"
        ))
        .expect("citation validation report schema");
        let validator = jsonschema::validator_for(&schema).expect("compile schema");

        assert!(validator.validate(&report).is_ok(), "{report:#}");
        assert_eq!(report["status"], "needs_remediation");
        assert_eq!(
            report["resolver_results"][0]["source"],
            "crossref_retraction"
        );
        assert_eq!(report["resolver_results"][0]["status"], "retracted");
        assert_eq!(
            report["resolver_results"][0]["evidence"][0],
            "update-to type=retraction doi=10.notice/retraction source=retraction-watch"
        );
    }

    #[test]
    fn artifact_provenance_names_the_function_or_tool_that_created_each_output() {
        let provenance = build_artifact_provenance(
            SourceBodyProducer::PandocTex,
            ExtractionMode::PandocEnabled,
            Some(&StageOutcome {
                output: json!({ "equations": [] }),
                tool_calls: Vec::new(),
                cost_usd: 0.0,
                latency_ms: 0,
                iters: 0,
                model: "pandoc-equation-scan".into(),
                runner: "rust_fn".into(),
                provenance: NodeProvenance::rust_tool(
                    "derive_equations",
                    "scan_equations",
                    ["body.md", "semantic_ast.json"],
                    ["equations.json"],
                    ExtractionMode::PandocEnabled,
                ),
            }),
            None,
            None,
        );

        let body = provenance
            .iter()
            .find(|entry| entry.artifact == "body.md")
            .expect("body.md provenance");
        assert_eq!(body.executor, ExecutorKind::Tool);
        assert_eq!(body.tool.as_deref(), Some("pandoc_tex_to_markdown"));

        let metadata = provenance
            .iter()
            .find(|entry| entry.artifact == "metadata.json")
            .expect("metadata provenance");
        assert_eq!(metadata.tool.as_deref(), Some("build_metadata_json"));

        let equations = provenance
            .iter()
            .find(|entry| entry.artifact == "equations.json")
            .expect("equations provenance");
        assert_eq!(equations.tool.as_deref(), Some("scan_equations"));
        assert_eq!(equations.mode, ExtractionMode::PandocEnabled);
    }

    #[test]
    fn deterministic_equation_fallback_extracts_pandoc_math() {
        let body = "## Spectral setup\n\n\
            Inline $a+b$ and display $$\\begin{equation}\nE=mc^2\n\\end{equation}$$.\n\n\
            \\[\\int_0^1 f(x)\\,dx\\]\n";
        let outcome = deterministic_equations_outcome("2605.00403", None, body)
            .expect("fallback should produce equations");
        assert_eq!(outcome.model, "pandoc-equation-scan");
        let equations = outcome.output["equations"].as_array().unwrap();
        assert_eq!(equations.len(), 3, "equations={equations:?}");
        assert_eq!(equations[0]["canonical_tex"], "a+b");
        assert_eq!(equations[1]["canonical_tex"], "E=mc^2");
        assert_eq!(equations[2]["canonical_tex"], "\\int_0^1 f(x)\\,dx");
    }

    #[test]
    fn deterministic_theorem_fallback_extracts_title_headings() {
        let body = "## Generalized Fourier Transform\n\n\
            ### Spectral Decomposition Theorem\n\nEvery self-adjoint operator has a spectral representation.\n\n\
            **Proposition.** The inverse transform follows from completeness.\n\n\
            ##### Proof sketch.\n\nApply Fubini and the resolution of identity.\n";
        let outcome = deterministic_theorems_outcome("2605.00403", body)
            .expect("fallback should produce theorem nodes");
        assert_eq!(outcome.model, "pandoc-theorem-scan");
        let nodes = outcome.output["nodes"].as_array().unwrap();
        assert!(
            nodes.len() >= 3,
            "expected theorem/proposition/proof nodes, got {nodes:?}"
        );
        assert!(nodes.iter().any(|n| n["type"] == "theorem"));
        assert!(nodes.iter().any(|n| n["type"] == "proposition"));
        assert!(nodes.iter().any(|n| n["type"] == "proof"));
    }

    #[test]
    fn deterministic_citation_fallback_groups_contexts() {
        let body = "## Intro\n\nA grouped citation [@Folland; @Spectral1] motivates this.\n\n\
            ## Results\n\nWe compare against [@Folland].\n";
        let mut extract = extract_with(vec!["Intro", "Results"], 0);
        extract.bibliography = vec![
            Citation {
                raw: "Folland: Folland reference".into(),
                doi: None,
                arxiv_id: None,
                title: Some("Folland reference".into()),
            },
            Citation {
                raw: "Spectral1: Spectral reference".into(),
                doi: None,
                arxiv_id: None,
                title: Some("Spectral reference".into()),
            },
        ];
        let outcome = deterministic_citations_outcome("2605.00403", body, &extract)
            .expect("fallback should produce citations");
        assert_eq!(outcome.model, "pandoc-citation-scan");
        let citations = outcome.output["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 2, "citations={citations:?}");
        let folland = citations
            .iter()
            .find(|c| c["key"] == "Folland")
            .expect("Folland citation");
        assert_eq!(folland["contexts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn deterministic_citation_fallback_preserves_uncited_bibliography_entries() {
        let body = "## Intro\n\nWe build on [@Barra:2012aa].\n";
        let mut extract = extract_with(vec!["Intro"], 0);
        extract.bibliography = vec![
            Citation {
                raw: "Barra:2012aa: Cited Paper".into(),
                doi: Some("10.1000/cited".into()),
                arxiv_id: None,
                title: Some("Cited Paper".into()),
            },
            Citation {
                raw: "uncited_key: Uncited Paper".into(),
                doi: Some("10.1000/uncited".into()),
                arxiv_id: None,
                title: Some("Uncited Paper".into()),
            },
        ];
        let outcome = deterministic_citations_outcome("2605.00403", body, &extract)
            .expect("fallback should produce citations");
        let citations = outcome.output["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 2, "citations={citations:?}");
        let cited = citations
            .iter()
            .find(|c| c["key"] == "Barra:2012aa")
            .expect("cited entry");
        assert_eq!(cited["cited"], true);
        assert_eq!(cited["contexts"].as_array().unwrap().len(), 1);
        let uncited = citations
            .iter()
            .find(|c| c["key"] == "uncited_key")
            .expect("uncited entry");
        assert_eq!(uncited["cited"], false);
        assert!(uncited["contexts"].as_array().unwrap().is_empty());
        assert_eq!(
            outcome.output["uncited_bibliography_keys"],
            json!(["uncited_key"])
        );
    }

    #[test]
    fn deterministic_citation_fallback_matches_author_year_pdf_citations() {
        let body = "## Intro\n\n\
            Agents completed tasks faster than humans (Wang et al. 2025). \
            A large field study found gains for support specialists \
            (Brynjolfsson, Li, and Raymond 2025). \
            Social support remains central (House and Kahn 1985; Cohen and Wills 1985).\n\n\
            ## References\n\n\
            Wang, Z. Z.; Shao, Y.; Shaikh, O.; Fried, D.; Neubig, G.; and Yang, D. 2025. \
            How Do AI Agents Do Human Work? arXiv:2510.22780.\n\
            Brynjolfsson, E.; Li, D.; and Raymond, L. 2025. Generative AI at work.\n\
            House, J.; and Kahn, R. 1985. Measures and concepts of social support.\n\
            Cohen, S.; and Wills, T. A. 1985. Stress, social support, and the buffering hypothesis.\n";
        let mut extract = extract_with(vec!["Intro"], 0);
        extract.bibliography = vec![
            Citation {
                raw: "Wang, Z. Z.; Shao, Y.; Shaikh, O.; Fried, D.; Neubig, G.; and Yang, D. 2025. How Do AI Agents Do Human Work?".into(),
                doi: None,
                arxiv_id: Some("2510.22780".into()),
                title: None,
            },
            Citation {
                raw: "Brynjolfsson, E.; Li, D.; and Raymond, L. 2025. Generative AI at work.".into(),
                doi: None,
                arxiv_id: None,
                title: None,
            },
            Citation {
                raw: "House, J.; and Kahn, R. 1985. Measures and concepts of social support.".into(),
                doi: None,
                arxiv_id: None,
                title: None,
            },
            Citation {
                raw: "Cohen, S.; and Wills, T. A. 1985. Stress, social support, and the buffering hypothesis.".into(),
                doi: None,
                arxiv_id: None,
                title: None,
            },
        ];

        let outcome = deterministic_citations_outcome("2605.22707", body, &extract)
            .expect("author-year fallback should produce citations");
        let citations = outcome.output["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 4, "citations={citations:?}");
        assert!(
            citations
                .iter()
                .all(|c| c["contexts"].as_array().unwrap().len() == 1),
            "citations={citations:?}"
        );
        assert!(outcome.output["unmatched_citation_keys"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn citation_key_does_not_treat_pdf_title_colon_as_bibtex_key() {
        let citation = Citation {
            raw: "Abbas, T.; Rathore, S. A.; and Daud, A. 2025. Enhancing Software Engineering With AI: Innovations, Challenges, and Future Directions.".into(),
            doi: None,
            arxiv_id: None,
            title: None,
        };
        assert_eq!(
            citation_key(&citation).as_deref(),
            Some("Abbas et al. 2025")
        );

        let keyed = Citation {
            raw: "Barra:2012aa: Cited Paper".into(),
            doi: None,
            arxiv_id: None,
            title: Some("Cited Paper".into()),
        };
        assert_eq!(citation_key(&keyed).as_deref(), Some("Barra:2012aa"));
    }

    #[test]
    fn low_quality_citation_agent_chunks_are_replaced_by_deterministic_output() {
        let oversized_raw = "A. Author. 2026. First.\n".repeat(80);
        let current = deterministic_empty_outcome(
            "citations",
            json!({
                "citations": [
                    { "key": "chunk1", "raw": oversized_raw, "contexts": [] },
                    { "key": "chunk2", "raw": "Small chunk", "contexts": [] }
                ],
                "unmatched_citation_keys": [],
                "uncited_bibliography_keys": []
            }),
        );
        let deterministic = deterministic_empty_outcome(
            "citations",
            json!({
                "citations": [
                    { "key": "ref1", "raw": "One", "contexts": [] },
                    { "key": "ref2", "raw": "Two", "contexts": [] },
                    { "key": "ref3", "raw": "Three", "contexts": [] }
                ],
                "unmatched_citation_keys": [],
                "uncited_bibliography_keys": []
            }),
        );

        assert!(should_replace_low_quality_citations(
            Some(&current),
            Some(&deterministic)
        ));
    }

    #[test]
    fn source_manifest_indexes_archive_and_extracts_figures() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut tar_bytes = Vec::new();
        {
            let encoder = GzEncoder::new(&mut tar_bytes, Compression::default());
            let mut builder = tar::Builder::new(encoder);
            append_tar_entry(&mut builder, "paper.tex", br"\documentclass{article}");
            append_tar_entry(&mut builder, "refs.bib", b"@article{x,title={X}}\n");
            append_tar_entry(&mut builder, "figs/fig_1.png", b"\x89PNG\r\n\x1a\n");
            builder.finish().unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let manifest =
            build_source_manifest_json("2605.19178", &tar_bytes).expect("source manifest");
        let paths: Vec<&str> = manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["path"].as_str())
            .collect();
        assert!(paths.contains(&"paper.tex"), "paths={paths:?}");
        assert!(paths.contains(&"refs.bib"), "paths={paths:?}");
        assert!(paths.contains(&"figs/fig_1.png"), "paths={paths:?}");
        let fig = manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "figs/fig_1.png")
            .expect("figure entry");
        assert_eq!(fig["kind"], "figure");

        let figures = extract_source_figures(&tar_bytes).expect("figures");
        assert_eq!(figures.len(), 1);
        assert_eq!(figures[0].0, "figs/fig_1.png");
        assert_eq!(figures[0].1, b"\x89PNG\r\n\x1a\n");
    }

    fn append_tar_entry(
        builder: &mut tar::Builder<impl std::io::Write>,
        path: &str,
        contents: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents)
            .expect("append tar entry");
    }
}
