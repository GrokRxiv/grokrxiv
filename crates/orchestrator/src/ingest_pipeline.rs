//! RPT3 Wave-3 — staged ingest pipeline orchestrator.
//!
//! Wires Stages 1–8 of the 8-stage extraction pipeline into a single
//! [`run_ingest_pipeline`] entry point:
//!
//! 1. **Stage 1 (deterministic)** — arXiv metadata + PDF + tar.gz acquisition.
//! 2. **Stage 2 (deterministic)** — TeX → Pandoc+LaTeXML → markdown + semantic AST.
//! 3. **Stage 3 (agent)** — `VlmExtractorAgent`, only when no TeX source exists.
//! 4. **Stages 4–7 (agents in parallel)** — macros, equations, theorems,
//!    citations. Failures degrade gracefully via [`run_agent_safe`] — a single
//!    agent crash never tanks the whole run.
//! 5. **Stage 8 (deterministic)** — `PaperArtifacts::persist` writes Tier 1
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
//! - Stages 4–7 each wrap their `Agent::run` call in [`run_agent_safe`].
//!   `Ok(_)` populates the matching `ArtifactBundle` field; `Err(_)` logs at
//!   `warn` level, records a `degraded` entry in `extraction_report.json`,
//!   and leaves the bundle field at its default (`None` for the optional
//!   `serde_json::Value` fields). Downstream stages see the missing artifact
//!   and either skip it (equations / theorems / references arrays empty) or
//!   fall back (normalized_tex falls back to raw_tex).
//! - Stage 8 failure is fatal: we flip `extraction_status = 'failed'` so the
//!   next run picks it back up.

#![cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use grokrxiv_ingest::{DeterministicIngest, PaperExtract};
use grokrxiv_storage::{
    ArtifactBundle, GitArtifactStore, PaperArtifacts, PersistedPointer, ReviewInput,
    SupabaseStorage,
};

use crate::agents::extraction::{
    citations::CitationContextualizerAgent, equations::EquationCanonicalizerAgent,
    macros::MacroExpanderAgent, theorems::TheoremGraphExtractorAgent, vlm::VlmExtractorAgent,
    ExtractionAgent, ToolRegistry,
};
use crate::agents::traits::AgentRunner;
use crate::agents::types::{AgentSpec, ExtractionContext};
use crate::db;
use crate::state::AppState;

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
        self.skip_stages.iter().any(|s| s.eq_ignore_ascii_case(stage))
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
            let _ = db::mark_paper_extraction_failed(pool, paper_id, &format!("{e:#}"))
                .await;
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
    let workdir = tempfile::tempdir().context("creating extraction workdir")?;
    let extract = staged.extract.clone();
    let mut bundle = build_initial_bundle(&staged, &extract);
    let mut stage_reports: Vec<StageReport> = Vec::new();
    stage_reports.push(StageReport::ok(
        "acquisition",
        None,
        None,
        Vec::new(),
    ));

    // Stage 2 reflection: whether deterministic TeX→AST produced semantic_ast.
    let semantic_ast = staged.semantic_ast;
    if semantic_ast.is_some() {
        stage_reports.push(StageReport::ok("tex_to_ast", None, None, Vec::new()));
    } else if staged.source_tarball.is_some() {
        stage_reports.push(StageReport {
            name: "tex_to_ast".into(),
            status: "degraded".into(),
            duration_ms: None,
            cost_usd: None,
            model: None,
            runner: None,
            warnings: vec!["LaTeXML produced no semantic_ast".into()],
        });
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

    // semantic_ast embedded into the bundle for Tier-2 routing decisions.
    if let Some(ast) = semantic_ast.as_ref() {
        if let Ok(bytes) = serde_json::to_vec_pretty(ast) {
            bundle.semantic_ast = Some(bytes);
        }
    }

    // --- Stage 3 (VLM) — only when no TeX path was available ---
    if !opts.should_skip("vlm") && staged.source_tarball.is_none() {
        let started = std::time::Instant::now();
        let vlm = VlmExtractorAgent::new();
        match run_agent_safe(
            &vlm,
            state,
            workdir.path(),
            &extract,
            semantic_ast.as_ref(),
            paper_id,
            arxiv_id,
            "vlm",
        )
        .await
        {
            Some(output) => {
                apply_vlm(&mut bundle, &output);
                stage_reports.push(StageReport::ok(
                    "vlm",
                    Some(started.elapsed().as_millis() as i64),
                    None,
                    Vec::new(),
                ));
            }
            None => {
                stage_reports.push(StageReport::degraded("vlm", "agent run failed"));
            }
        }
    } else if staged.source_tarball.is_some() {
        stage_reports.push(StageReport::skipped("vlm", "tex path active"));
    }

    // --- Stages 4–7 in parallel ---
    let semantic_ast_ref = semantic_ast.as_ref();
    let (macros_res, equations_res, theorems_res, citations_res) = tokio::join!(
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
    );

    record_agent_outcome(&mut stage_reports, "macros", macros_res.as_ref(), opts);
    record_agent_outcome(&mut stage_reports, "equations", equations_res.as_ref(), opts);
    record_agent_outcome(&mut stage_reports, "theorems", theorems_res.as_ref(), opts);
    record_agent_outcome(&mut stage_reports, "citations", citations_res.as_ref(), opts);

    apply_equations(&mut bundle, arxiv_id, equations_res);
    apply_theorems(&mut bundle, arxiv_id, theorems_res);
    apply_citations(&mut bundle, arxiv_id, citations_res);
    apply_macros(&mut bundle, macros_res); // currently records to extraction_report

    // --- Stage 8: persist + finalise ---
    let completed_at = Utc::now();
    let report = json!({
        "arxiv_id": arxiv_id,
        "started_at": started_at.to_rfc3339(),
        "completed_at": completed_at.to_rfc3339(),
        "total_cost_usd": null,
        "stages": stage_reports.iter().map(StageReport::to_value).collect::<Vec<_>>(),
    });
    bundle.extraction_report = Some(report);

    let stages_run: Vec<&str> = stage_reports
        .iter()
        .filter(|s| s.status == "ok")
        .map(|s| s.name.as_str())
        .collect();

    let paper_artifacts = build_paper_artifacts(opts)?;
    let pointer = paper_artifacts
        .persist(
            paper_id.to_string(),
            bundle.clone(),
            &stages_run,
            None,
        )
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
        .unwrap_or_else(|| {
            PathBuf::from("/Users/mlong/Documents/Development/grokrxiv-data")
        });
    let remote = std::env::var("GROKRXIV_DATA_REPO_REMOTE").ok();
    let git =
        GitArtifactStore::open_or_clone(repo_path, remote).context("open grokrxiv-data repo")?;

    let dry_run = opts.dry_run_storage
        || matches!(std::env::var("GROKRXIV_DRY_RUN_STORAGE").as_deref(), Ok("1"));
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

fn build_initial_bundle(
    staged: &DeterministicIngest,
    extract: &PaperExtract,
) -> ArtifactBundle {
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
            let key = format!("ref{i}");
            json!({
                "key": key,
                "title": c.title,
                "authors": Vec::<String>::new(),
                "venue": Value::Null,
                "year": Value::Null,
                "doi": c.doi,
                "arxiv_id": c.arxiv_id,
                "contexts": Vec::<Value>::new(),
            })
        })
        .collect();
    json!({ "arxiv_id": arxiv_id, "citations": citations })
}

fn unpack_tarball(workdir: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Cursor;
    let mut decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(&mut decoder);
    archive.unpack(workdir).context("unpack tar.gz")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent runner wrappers
// ---------------------------------------------------------------------------

fn resolve_runner(state: &AppState) -> Option<Arc<dyn AgentRunner>> {
    // Extraction agents currently share the "Api" runner with the review DAG.
    // Cli / cloud / local_inference runners are not yet plumbed for the
    // extraction tool-call loop, so we deliberately pin to Api here.
    state
        .runners
        .get(&crate::agents::types::AgentRunnerKind::Api)
        .cloned()
}

/// Per-stage routing read from `agents/extraction/<role>.yaml`. Captures the
/// fields the extraction tool-loop needs at runtime.
#[derive(serde::Deserialize, Clone, Debug)]
struct ExtractionRouting {
    provider: String,
    model: String,
    #[serde(default)]
    runner: Option<crate::agents::types::AgentRunnerKind>,
    #[serde(default)]
    max_cost_usd: Option<f32>,
    #[serde(default)]
    max_iters: Option<u32>,
    #[serde(default)]
    timeout_secs: Option<u32>,
    #[serde(default)]
    max_retries: Option<u8>,
}

/// Load `agents/extraction/<stage>.yaml`. Lazy-cached per-stage so repeated
/// agent invocations within a single ingest don't re-read the file each time.
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

    // Stage names from ingest_pipeline use short forms ("macros", "equations",
    // "theorems", "citations", "vlm"); the YAML files use the same. Resolve
    // relative to `GROKRXIV_AGENTS_DIR` (matching state.rs) or `./agents/`.
    let agents_dir = std::env::var("GROKRXIV_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("agents"));
    let path = agents_dir.join("extraction").join(format!("{stage}.yaml"));
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

/// Per-stage budget bundle (resolved from YAML with fallbacks). Surfaced
/// separately from `AgentSpec` because the tool-loop's cost/iter ceilings live
/// on its `run_tool_loop` args, not on the spec.
#[allow(dead_code)]
pub(crate) struct ExtractionBudget {
    pub max_cost_usd: f32,
    pub max_iters: u32,
}

/// Per-stage budget bundle (resolved from YAML with fallbacks). Not yet
/// threaded through `ExtractionAgent::run` — each agent currently hardcodes
/// its own ceilings inline. Kept as a hook so a follow-up pass can replace
/// the inline numbers with this YAML-honored bundle without re-loading the
/// YAML in each agent module.
#[allow(dead_code)]
pub(crate) fn extraction_budget_for(stage: &str) -> ExtractionBudget {
    let routing = load_extraction_routing(stage);
    let (cost, iters) = match stage {
        "vlm" => (1.0, 40),
        "macros" => (0.20, 20),
        "equations" => (0.50, 60),
        "theorems" => (0.80, 50),
        "citations" => (0.50, 80),
        _ => (0.50, 40),
    };
    ExtractionBudget {
        max_cost_usd: routing
            .as_ref()
            .and_then(|r| r.max_cost_usd)
            .unwrap_or(cost),
        max_iters: routing.as_ref().and_then(|r| r.max_iters).unwrap_or(iters),
    }
}

fn default_extraction_spec(role: &str) -> AgentSpec {
    use grokrxiv_schemas::AgentRole;
    // The review-side AgentRole enum doesn't have extraction variants — we
    // reuse `Summary` as a stable seed since the role field is only used for
    // logging/observability inside the spec. The provider+model come from
    // `agents/extraction/<role>.yaml` so each stage actually hits its
    // configured backend (e.g. citations.yaml -> gemini-2.5-flash, not the
    // claude-haiku-4-5 fallback the audit caught us hardcoding).
    let routing = load_extraction_routing(role);
    let (provider, model) = match routing.as_ref() {
        Some(r) => (r.provider.clone(), r.model.clone()),
        None => (
            "claude".to_string(),
            std::env::var("GROKRXIV_EXTRACTION_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5".to_string()),
        ),
    };
    let mut spec = AgentSpec::api_default(AgentRole::Summary, provider, model);
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
) -> Option<Value> {
    let Some(runner) = resolve_runner(state) else {
        warn!(stage_name, "no runner available; skipping extraction stage");
        return None;
    };
    let registry = Arc::new(build_registry_for(agent));
    let ctx = ExtractionContext {
        workdir,
        extract,
        semantic_ast,
        paper_id,
        arxiv_id,
        registry,
    };
    let spec = default_extraction_spec(stage_name);
    match agent.run(runner, &spec, ctx).await {
        Ok(run) => {
            info!(stage_name, arxiv_id, iters = run.iters, "extraction stage ok");
            Some(run.output)
        }
        Err(e) => {
            warn!(stage_name, arxiv_id, err = %format!("{e:#}"), "extraction stage failed");
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
) -> Option<Value> {
    if !enabled {
        return None;
    }
    run_agent_safe(
        &agent, state, workdir, extract, semantic_ast, paper_id, arxiv_id, stage_name,
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
        crate::agents::extraction::vlm::build_registry()
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
            crate::agents::extraction::citations::tools::ListCitationSitesTool,
        ));
        r.register(Arc::new(
            crate::agents::extraction::citations::tools::LookupBibtexTool,
        ));
        r.register(Arc::new(
            crate::agents::extraction::citations::tools::SearchCorpusTool,
        ));
        r.register(Arc::new(
            crate::agents::extraction::citations::tools::ReadSectionTool,
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

fn apply_equations(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<Value>) {
    let Some(value) = res else {
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

fn apply_theorems(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<Value>) {
    let Some(value) = res else {
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

fn apply_citations(bundle: &mut ArtifactBundle, arxiv_id: &str, res: Option<Value>) {
    let Some(value) = res else {
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
                        "title": c.get("title").cloned().unwrap_or(Value::Null),
                        "authors": c.get("authors").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
                        "venue": c.get("venue").cloned().unwrap_or(Value::Null),
                        "year": c.get("year").cloned().unwrap_or(Value::Null),
                        "doi": c.get("doi").or_else(|| c.get("resolved_doi")).cloned().unwrap_or(Value::Null),
                        "arxiv_id": c.get("arxiv_id").or_else(|| c.get("resolved_arxiv_id")).cloned().unwrap_or(Value::Null),
                        "contexts": contexts,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    bundle.references = Some(json!({ "arxiv_id": arxiv_id, "citations": citations }));
}

fn apply_macros(_bundle: &mut ArtifactBundle, _res: Option<Value>) {
    // The MacroExpander returns `{normalized_tex, expansions_applied}`. We
    // don't currently re-route normalized_tex back into Stage 5/6/7 because
    // those agents read from `body.md` + `semantic_ast` (already produced by
    // Stage 2). The result is recorded in `extraction_report.json` via the
    // stage outcome bookkeeping.
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
        }
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
            "tool_call_summary": Value::Null,
        })
    }
}

fn record_agent_outcome(
    out: &mut Vec<StageReport>,
    name: &str,
    res: Option<&Value>,
    opts: &IngestOptions,
) {
    if opts.should_skip(name) {
        out.push(StageReport::skipped(name, "skipped via --skip-stages"));
        return;
    }
    if res.is_some() {
        out.push(StageReport::ok(name, None, None, Vec::new()));
    } else {
        out.push(StageReport::degraded(name, "agent returned no output"));
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
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    doi: c.get("doi").and_then(Value::as_str).map(str::to_string),
                    arxiv_id: c.get("arxiv_id").and_then(Value::as_str).map(str::to_string),
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
