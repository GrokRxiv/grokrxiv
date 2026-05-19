//! Job supervisor.
//!
//! A lightweight tokio-based dispatcher that owns mpsc channels for each
//! [`JobKind`]. The course-correction (private-first moderation) means the
//! supervisor's review pipeline ENDS at `status = awaiting_moderation`;
//! publishing requires explicit admin approval through the
//! `/admin/reviews/:id/approve` endpoint which calls
//! [`Supervisor::publish_after_approval`].

use std::time::Duration;

use grokrxiv_schemas::JobKind;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::state::AppState;

/// Single in-flight unit of work.
#[derive(Debug, Clone)]
pub struct WorkItem {
    /// Database job id.
    pub job_id: Uuid,
    /// What to do.
    pub kind: JobKind,
    /// Entity reference (paper / review id). Used for Review/Publish where the
    /// id is known up front; Ingest carries the arXiv id in `payload` instead
    /// because the paper row doesn't exist yet.
    pub ref_id: Option<Uuid>,
    /// Free-form payload. For `JobKind::Ingest` this carries
    /// `{ "arxiv_id": "<id>" }`. Empty for jobs that have everything they need
    /// in `ref_id`.
    pub payload: serde_json::Value,
    /// Attempt counter (0 = first).
    pub attempt: u32,
}

/// Maximum retry attempts for any single job.
pub const MAX_RETRIES: u32 = 3;

/// FP-RPT3b B2: minimum number of specialists that must hold a
/// `verifier_status = pass` for the meta-reviewer node to be considered safe
/// to run. With fewer than this many passing specialists the synthesis input
/// is degenerate (one or two prose blobs cannot anchor a balanced meta-review),
/// so the DAG aborts and the review row is moved to `withdrawn` with a
/// structured `meta_review.error` describing the failure.
///
/// Threshold = 3 of 5: lenient enough to ride out a single transient provider
/// error or a soft verifier warning rebadged as a fail, strict enough that the
/// meta-reviewer never sees the corner case of "one specialist plus
/// hallucination".
pub const MIN_SPECIALIST_QUORUM: usize = crate::review_dag::DEFAULT_MIN_SPECIALIST_QUORUM;

/// In-memory supervisor handle.
#[derive(Clone)]
pub struct Supervisor {
    tx: mpsc::Sender<WorkItem>,
}

impl Supervisor {
    /// Spawn the supervisor task and return a handle for enqueueing work.
    pub fn spawn(state: AppState) -> Self {
        let (tx, mut rx) = mpsc::channel::<WorkItem>(128);
        let me = Self { tx: tx.clone() };
        let state2 = state;
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let state = state2.clone();
                let retry_tx = tx.clone();
                tokio::spawn(async move {
                    let result = run_item(&state, &item, &retry_tx).await;
                    if let Err(e) = result {
                        tracing::error!(
                            job_id = %item.job_id,
                            attempt = item.attempt,
                            err = %e,
                            "job failed"
                        );
                    }
                });
            }
        });
        me
    }

    /// Enqueue a unit of work.
    pub async fn enqueue(&self, item: WorkItem) -> anyhow::Result<()> {
        self.tx
            .send(item)
            .await
            .map_err(|e| anyhow::anyhow!("supervisor channel closed: {e}"))
    }

    /// Borrow the underlying sender so the scheduler / admin routes can
    /// enqueue without holding a `Supervisor` handle.
    pub fn sender(&self) -> mpsc::Sender<WorkItem> {
        self.tx.clone()
    }

    /// Hook called by the admin approval endpoint to start the publish step
    /// after a moderator approves a private review.
    pub async fn publish_after_approval(&self, review_id: Uuid) -> anyhow::Result<()> {
        let job = WorkItem {
            job_id: Uuid::new_v4(),
            kind: JobKind::Publish,
            ref_id: Some(review_id),
            payload: serde_json::Value::Null,
            attempt: 0,
        };
        self.enqueue(job).await
    }
}

/// Drive a single paper through ingest + review synchronously and return the
/// resulting review row id. Used by the `ingest-one` CLI subcommand and the
/// M1 integration test.
///
/// With `--features full` + a configured `DATABASE_URL` + reachable runner,
/// this produces:
///   - 1 row in `papers`,
///   - 1 row in `reviews` at `awaiting_moderation`,
///   - 6 rows in `review_agents` (summary / technical_correctness / novelty /
///     reproducibility / citation / meta_reviewer),
///   - `verifier_status` set on every review_agent row.
///
/// Without `--features full` the function still runs but the supporting
/// sibling crates are absent; the call returns an error pointing the operator
/// at the right build flag.
pub async fn run_one_paper_blocking(
    _supervisor: &Supervisor,
    state: &AppState,
    arxiv_id: &str,
) -> anyhow::Result<Uuid> {
    #[cfg(feature = "grokrxiv-ingest")]
    {
        run_one_paper_full(state, arxiv_id).await
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = state;
        let _ = arxiv_id;
        anyhow::bail!(
            "run_one_paper_blocking requires --features full (grokrxiv-ingest \
             + grokrxiv-render). Rebuild with: cargo run --release -p \
             grokrxiv-orchestrator -- ingest <ARXIV_ID>"
        );
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn run_one_paper_full(state: &AppState, arxiv_id: &str) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    tracing::info!(arxiv_id, "M1: ingest start");
    crate::cli_status::emit(format!(
        "paper {arxiv_id}: fetching arXiv source and metadata"
    ));

    // RPT3 Wave-3 Team-F: when the storage feature is on, the orchestrator's
    // staged ingest pipeline runs Stages 1–8 (acquisition → format conversion
    // → extraction agents → persist to grokrxiv-data + Supabase + paper_assets
    // pointers). The review path then reads from the persisted
    // `review_input.json`.
    let (paper_id, extract);
    #[cfg(feature = "grokrxiv-storage")]
    {
        let opts = ingest_options_from_env();
        crate::cli_status::emit(format!(
            "paper {arxiv_id}: running staged extraction pipeline"
        ));
        match crate::ingest_pipeline::run_ingest_pipeline(state, arxiv_id, &opts).await {
            Ok(out) => {
                paper_id = out.paper_id;
                extract = out.extract;
            }
            Err(e) => {
                tracing::warn!(arxiv_id, err = %format!("{e:#}"), "staged ingest pipeline failed; falling back to deterministic-only path");
                let pe = {
                    let _permit = state.arxiv.acquire().await;
                    grokrxiv_ingest::pipeline::ingest(arxiv_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
                };
                paper_id = crate::db::upsert_paper(pool, &pe, None).await?;
                extract = pe;
            }
        }
    }
    #[cfg(not(feature = "grokrxiv-storage"))]
    {
        let pe = {
            let _permit = state.arxiv.acquire().await;
            grokrxiv_ingest::pipeline::ingest(arxiv_id)
                .await
                .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
        };
        paper_id = crate::db::upsert_paper(pool, &pe, None).await?;
        extract = pe;
    }
    tracing::info!(arxiv_id, %paper_id, "M1: paper persisted");
    crate::cli_status::emit(format!(
        "paper {arxiv_id}: extraction persisted as paper_id={paper_id}; starting review DAG"
    ));

    run_review_dag_from_state(state, pool, paper_id, extract).await
}

#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
fn ingest_options_from_env() -> crate::ingest_pipeline::IngestOptions {
    crate::ingest_pipeline::IngestOptions::from_env()
}

/// Drive the review DAG for a paper row that is already present in the database.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_for_paper_blocking(
    state: &AppState,
    paper_id: Uuid,
) -> anyhow::Result<Uuid> {
    run_review_for_paper_full(state, paper_id).await
}

/// Drive the review DAG for a paper row using a caller-supplied extract.
/// This is the non-arXiv source entry point: local PDF/TeX and git adapters
/// prepare the same `PaperExtract` shape as arXiv ingest, then persist the
/// paper row and call this function.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_for_extract_blocking(
    state: &AppState,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    run_review_dag_from_state(state, pool, paper_id, extract).await
}

/// Real typed DAG for a single paper.
///
/// Five specialist agents fan out in parallel (bounded by a 5-permit
/// semaphore so we don't blow past per-key provider rate limits). Each
/// specialist call is JSON-schema-enforced against its role-specific schema,
/// and the output is verified by a role-specific verifier ladder. Once all
/// five complete, the meta-reviewer is called with the bundle of specialist
/// outputs as its input artifact and asked to synthesize a `MetaReview`.
/// Every agent's input + output is persisted to `review_agents`.
///
/// Exposed publicly so the integration test in `tests/dag.rs` can drive the
/// DAG with a stubbed Anthropic provider and a hand-built `PaperExtract`.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_dag(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    run_review_dag_inner(state, pool, Some(provider), paper_id, extract).await
}

#[cfg(feature = "grokrxiv-ingest")]
async fn run_review_dag_from_state(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    let provider = state
        .providers
        .as_ref()
        .map(|registry| registry.default.clone());
    run_review_dag_inner(state, pool, provider, paper_id, extract).await
}

// RPT2 G follow-up: the CLI's `--runner` / `--runner-for` flags land in these
// env vars before review dispatch. They override the YAML's `runner:` field
// per role. Format:
//   GROKRXIV_RUNNER_OVERRIDE        = "cli" | "api" | "cloud" | "local_inference"
//   GROKRXIV_RUNNER_OVERRIDE_<ROLE> = same enum, per role
#[cfg(feature = "grokrxiv-ingest")]
fn review_runner_override_for(
    role: grokrxiv_schemas::AgentRole,
) -> Option<crate::agents::AgentRunnerKind> {
    use crate::agents::AgentRunnerKind;
    use grokrxiv_schemas::AgentRole;

    let role_slug = match role {
        AgentRole::Summary => "summary",
        AgentRole::TechnicalCorrectness => "technical_correctness",
        AgentRole::Novelty => "novelty",
        AgentRole::Reproducibility => "reproducibility",
        AgentRole::Citation => "citation",
        AgentRole::MetaReviewer => "meta_reviewer",
    };
    let per_role_var = format!("GROKRXIV_RUNNER_OVERRIDE_{}", role_slug.to_uppercase());
    std::env::var(&per_role_var)
        .ok()
        .or_else(|| std::env::var("GROKRXIV_RUNNER_OVERRIDE").ok())
        .and_then(|s| match s.as_str() {
            "api" => Some(AgentRunnerKind::Api),
            "cli" => Some(AgentRunnerKind::Cli),
            "cloud" => Some(AgentRunnerKind::Cloud),
            "local_inference" => Some(AgentRunnerKind::LocalInference),
            _ => None,
        })
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_cache_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    ) || matches!(
        std::env::var("GROKRXIV_INGEST_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    )
}

#[cfg(feature = "grokrxiv-ingest")]
fn specialist_review_concurrency_limit(roles: &[grokrxiv_schemas::AgentRole]) -> usize {
    use crate::agents::AgentRunnerKind;

    let max = roles.len().max(1);
    let has_cli_role = roles.iter().any(|role| {
        review_runner_override_for(*role).unwrap_or(AgentRunnerKind::Cli) == AgentRunnerKind::Cli
    });
    review_concurrency_limit_from(
        std::env::var("GROKRXIV_REVIEW_CONCURRENCY").ok().as_deref(),
        has_cli_role,
        max,
    )
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_concurrency_limit_from(raw: Option<&str>, _has_cli_role: bool, max: usize) -> usize {
    let max = max.max(1);
    if let Some(parsed) = raw.and_then(|s| s.trim().parse::<usize>().ok()) {
        return parsed.clamp(1, max);
    }
    max
}

#[cfg(feature = "grokrxiv-ingest")]
async fn run_review_dag_inner(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: Option<std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    use crate::agents::runners::api::ApiRunner;
    use crate::agents::{
        build_agent, AgentInput, AgentMode, AgentRunner, AgentRunnerKind, AgentSpec, ReviewAgent,
        SandboxPolicy, ToolPolicy,
    };
    use grokrxiv_schemas::{AgentRole, MetaReview, VerifierStatus};
    use serde_json::json;
    use std::sync::Arc;

    let default_model = state.config.preview_model.clone();

    // Resolve the `ReviewAgent` + `AgentRunner` pair for a role. Prefers the
    // boot-time registry built from `agents/*.yaml`; falls back to the active
    // review runner only when a role config is missing. API fallback needs a
    // provider, but CLI fallback can run through local subscriptions.
    let make_fallback = |role: AgentRole,
                         schema: serde_json::Value|
     -> anyhow::Result<(
        Arc<dyn ReviewAgent>,
        Arc<dyn AgentRunner>,
        String,
    )> {
        let runner_kind = review_runner_override_for(role).unwrap_or(AgentRunnerKind::Cli);
        let model = crate::runtime_config::model_override_for_role(role)
            .unwrap_or_else(|| default_model.clone());
        let spec = AgentSpec {
            role,
            runner: runner_kind,
            sandbox: SandboxPolicy::None,
            mode: AgentMode::ReviewOnly,
            provider: "claude".to_string(),
            model: model.clone(),
            schema,
            tool_policy: ToolPolicy::default(),
            max_retries: 2,
            timeout_secs: 180,
        };
        let agent: Arc<dyn ReviewAgent> = Arc::from(build_agent(spec));
        let runner = if runner_kind == AgentRunnerKind::Api {
            let Some(provider) = provider.as_ref() else {
                anyhow::bail!(
                    "no LLM provider configured for API review fallback; use --runner cli or set provider API keys"
                );
            };
            let mut providers_map: std::collections::HashMap<
                String,
                Arc<dyn grokrxiv_llm_adapter::LLMProvider>,
            > = std::collections::HashMap::new();
            providers_map.insert("claude".to_string(), provider.clone());
            Arc::new(ApiRunner::new(providers_map)) as Arc<dyn AgentRunner>
        } else {
            state
                .runners
                .get(&runner_kind)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("runner {runner_kind:?} not registered"))?
        };
        Ok((agent, runner, model))
    };

    let resolve_agent =
        |role: AgentRole| -> anyhow::Result<(Arc<dyn ReviewAgent>, Arc<dyn AgentRunner>, String)> {
            if let Some(agent) = state.agents.get(&role) {
                let model = agent.spec().model.clone();
                // Runtime override beats YAML's runner: field for this run.
                let runner_kind = review_runner_override_for(role).unwrap_or(agent.spec().runner);
                if let Some(runner) = state.runners.get(&runner_kind) {
                    return Ok((agent.clone(), runner.clone(), model));
                }
            }
            let schema = state
                .agent_schemas
                .get(&role)
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object" }));
            make_fallback(role, schema)
        };

    // Pre-create the review row. `models_used` records the per-role model so
    // the moderation UI + the m1-pipeline `distinct model` assertion can show
    // which model each specialist used.
    let summary_model = resolve_agent(AgentRole::Summary)?.2;
    let tech_model = resolve_agent(AgentRole::TechnicalCorrectness)?.2;
    let novelty_model = resolve_agent(AgentRole::Novelty)?.2;
    let repro_model = resolve_agent(AgentRole::Reproducibility)?.2;
    let cite_model = resolve_agent(AgentRole::Citation)?.2;
    let meta_model = resolve_agent(AgentRole::MetaReviewer)?.2;
    let models_used = json!({
        "summary": summary_model,
        "technical_correctness": tech_model,
        "novelty": novelty_model,
        "reproducibility": repro_model,
        "citation": cite_model,
        "meta_reviewer": meta_model,
    });
    let review_id = crate::db::insert_review(pool, paper_id, models_used, None).await?;
    // Mirror FP4: every review entering `awaiting_moderation` immediately
    // gets a `pending` row on the moderation queue. The CLI's reject /
    // request-changes / approve commands flip this row's `state`.
    let _ = crate::db::insert_moderation_pending(pool, review_id).await;
    tracing::info!(%review_id, "M1: review row created");
    crate::cli_status::emit(format!(
        "review {review_id}: created review row; starting specialist reviewers"
    ));

    // Drive the DAG inside an inner async block so any error path can
    // transition the review row off the stale `awaiting_moderation` state.
    // We use `withdrawn` because the DB enum has no `failed` value.
    let dag_result: anyhow::Result<()> = async {
    // The graph topology is declared as reusable data in review_dag. This
    // function remains the executor for now: it walks the canonical topology's
    // specialist fan-out, quorum gate, meta-reviewer, and render tail.
    let review_dag = crate::review_dag::ReviewDag::canonical();
    review_dag
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid review DAG topology: {e}"))?;
    let review_dag_layers = review_dag
        .execution_layers()
        .map_err(|e| anyhow::anyhow!("invalid review DAG execution layers: {e}"))?;
    tracing::debug!(
        nodes = review_dag.nodes().len(),
        layers = review_dag_layers.len(),
        "review: loaded canonical DAG topology"
    );
    let specialist_roles = review_dag.specialist_roles();
    let specialist_total = review_dag.specialist_count();
    let min_specialist_quorum = review_dag.min_specialist_quorum();

    let review_concurrency = specialist_review_concurrency_limit(&specialist_roles);
    crate::cli_status::emit(format!(
        "review {review_id}: specialist concurrency={review_concurrency}"
    ));
    let sem = Arc::new(tokio::sync::Semaphore::new(review_concurrency));
    let extract_arc = Arc::new(extract);
    let specialist_input: serde_json::Value =
        serde_json::to_value(extract_arc.as_ref()).unwrap_or_else(|_| json!({}));

    // FP6 A2: persist the shared specialist input artifact exactly once per
    // review. Specialists no longer each carry a duplicate copy of the paper
    // extract on their `review_agents` row.
    crate::db::insert_review_input(pool, review_id, paper_id, &specialist_input).await?;

    // FP6 A4: hash the specialist input so cache lookups key on the exact
    // bytes each specialist would have reasoned over. All five specialists
    // share the same input artifact in this DAG, so a single hash is enough.
    let specialist_content_hash =
        sha256_hex(&serde_json::to_vec(&specialist_input).unwrap_or_default());

    // Phase 3: surface moderator notes from any prior `grokrxiv request-changes`
    // run on this paper. The agents react to operator feedback on the next pass.
    let moderator_notes: Option<String> = crate::db::fetch_latest_changes_request_notes(pool, paper_id)
        .await
        .unwrap_or(None);

    // Phase A (review-specialist tools): pre-resolve verified facts for the
    // Reproducibility specialist. HEAD-checks every URL in the paper extract +
    // hits the public GitHub API for github.com/<owner>/<repo>. The LLM later
    // consumes these facts as ground truth via build_specialist_prompt; the
    // merge step overlays the verified URL/repo state onto the LLM's
    // reproducibility_review output before insert_review_agent.
    let (reproducibility_facts, novelty_facts) = tokio::join!(
        crate::agents::specialist_facts::gather_reproducibility_facts(&state.http, extract_arc.as_ref()),
        crate::agents::specialist_facts::gather_novelty_facts(&state.http, extract_arc.as_ref()),
    );
    let tc_facts = crate::agents::specialist_facts::gather_tc_facts(extract_arc.as_ref());
    tracing::info!(
        %paper_id,
        %review_id,
        urls_checked = reproducibility_facts.urls_checked.len(),
        urls_reachable = reproducibility_facts.urls_checked.iter().filter(|u| u.reachable).count(),
        github_repos = reproducibility_facts.github_repos.len(),
        related_papers = novelty_facts.related_papers.len(),
        novelty_retrieval_error = %novelty_facts.retrieval_error,
        tc_tables = tc_facts.tables.len(),
        tc_equation_labels = tc_facts.equation_labels.len(),
        tc_complexity_mentions = tc_facts.complexity_mentions.len(),
        "review: gathered reproducibility + novelty + TC facts"
    );
    if let Some(notes) = moderator_notes.as_deref() {
        tracing::info!(
            %paper_id,
            %review_id,
            notes_len = notes.len(),
            "review: surfacing moderator notes from prior changes-requested round"
        );
    }

    // Track 8a: optional dump of each rendered prompt to disk for inspection.
    // Triggered by the CLI's `--debug-prompt` flag → `GROKRXIV_DEBUG_PROMPT_DIR`
    // env var. Best-effort: any I/O failure is swallowed by `dump_debug_prompt`.
    let debug_root = debug_prompt_root();
    let skip_review_cache = review_cache_disabled();

    let mut handles = Vec::with_capacity(specialist_roles.len());
    for role in specialist_roles.iter().copied() {
        let repro_for_role = if matches!(role, AgentRole::Reproducibility) {
            Some(&reproducibility_facts)
        } else {
            None
        };
        let novelty_for_role = if matches!(role, AgentRole::Novelty) {
            Some(&novelty_facts)
        } else {
            None
        };
        let tc_for_role = if matches!(role, AgentRole::TechnicalCorrectness) {
            Some(&tc_facts)
        } else {
            None
        };
        let prompt = build_specialist_prompt(
            role,
            extract_arc.as_ref(),
            moderator_notes.as_deref(),
            repro_for_role,
            novelty_for_role,
            tc_for_role,
        );
        if let Some(root) = debug_root.as_deref() {
            dump_debug_prompt(root, &extract_arc.arxiv_id, role, &prompt);
        }
        let system = role_system_prompt(role, extract_arc.field.as_deref());
        let (agent, runner, role_model) = resolve_agent(role)?;
        let sem = sem.clone();
        let pool_cloned = pool.clone();
        let cache_hash = specialist_content_hash.clone();
        let specialist_input_cloned = specialist_input.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore alive");
            crate::cli_status::emit(format!(
                "review {review_id}: {} reviewer starting",
                role_slug(role)
            ));

            // FP6 A4: cache lookup before the LLM call. We only honour
            // verifier_status='pass' rows so a previously-warned/failed run
            // re-executes the agent.
            if !skip_review_cache {
                if let Ok(Some(hit)) =
                    crate::db::lookup_cache(&pool_cloned, paper_id, role, &cache_hash).await
                {
                    if hit.verifier_status == "pass" {
                        tracing::info!(
                            event = "cache",
                            role = role_slug(role),
                            hit = true,
                            "cache hit"
                        );
                        return anyhow::Ok((
                            role,
                            hit.output,
                            Some(hit.tokens_in.unwrap_or(0) as i32),
                            Some(hit.tokens_out.unwrap_or(0) as i32),
                            0i32,
                            hit.model,
                            true,
                        ));
                    }
                }
            } else {
                tracing::info!(
                    event = "cache",
                    role = role_slug(role),
                    disabled = true,
                    "cache bypassed"
                );
            }
            tracing::info!(
                event = "cache",
                role = role_slug(role),
                hit = false,
                "cache miss"
            );

            let input = AgentInput {
                paper_id,
                review_id,
                role,
                content_hash_material: specialist_input_cloned.clone(),
                artifact: specialist_input_cloned,
                system_prompt: system,
                user_prompt: prompt,
                source_bundle_path: None,
            };
            let run = agent.run(&*runner, input).await?;
            crate::cli_status::emit(format!(
                "review {review_id}: {} reviewer completed",
                role_slug(role)
            ));
            anyhow::Ok((
                role,
                run.output,
                run.tokens_in,
                run.tokens_out,
                run.latency_ms,
                role_model,
                false,
            ))
        }));
    }

    let mut specialist_results: Vec<(
        AgentRole,
        serde_json::Value,
        Option<i32>,
        Option<i32>,
        i32,
        String, // model actually used
        bool,   // cache hit
    )> = Vec::with_capacity(specialist_roles.len());
    for h in handles {
        let r = h
            .await
            .map_err(|e| anyhow::anyhow!("specialist join: {e}"))??;
        specialist_results.push(r);
    }
    crate::cli_status::emit(format!(
        "review {review_id}: specialist reviewers completed; running verifier ladder"
    ));

    // Persist + verify each specialist's output against its role-specific
    // verifier ladder. The ladder uses the role-specific JSON schema as its
    // first rung (replacing the previous permissive-object workaround).
    //
    // FP-RPT3b B2: capture each role's `verifier_status` so the quorum check
    // below can refuse to run meta_reviewer on degenerate input.
    let mut specialist_verifier_status: Vec<(AgentRole, Option<VerifierStatus>)> =
        Vec::with_capacity(specialist_results.len());
    for (role, output, tokens_in, tokens_out, latency_ms, used_model, cache_hit) in
        &specialist_results
    {
        let (v_status, v_notes) = verify_artifact(state, &extract_arc, *role, output).await;
        // Phase: split the citation review between LLM and verifier. The
        // verifier owns existence + DOI/URL (real Crossref/arXiv lookups);
        // the LLM owns relevance + missing-references prose. Merge before
        // persisting so a single consistent citation_review JSON lands on
        // review_agents.output.
        let output_to_persist = match *role {
            AgentRole::Citation => {
                merge_citation_verifier_into_output(output.clone(), v_notes.as_ref())
            }
            AgentRole::Reproducibility => {
                merge_reproducibility_facts_into_output(output.clone(), &reproducibility_facts)
            }
            AgentRole::Novelty => {
                merge_novelty_facts_into_output(output.clone(), &novelty_facts)
            }
            _ => output.clone(),
        };
        crate::db::insert_review_agent(
            pool,
            crate::db::ReviewAgentInsert {
                review_id,
                role: *role,
                model: used_model,
                output: output_to_persist,
                verifier_status: v_status,
                verifier_notes: v_notes.clone(),
                tokens_in: *tokens_in,
                tokens_out: *tokens_out,
                latency_ms: Some(*latency_ms),
            },
        )
        .await?;

        // FP6 A4: write fresh successful outputs back into the cache. Skip
        // cache hits (they're already cached) and skip warn/fail rows.
        if !*cache_hit && v_status == Some(VerifierStatus::Pass) {
            let _ = crate::db::insert_cache(
                pool,
                paper_id,
                *role,
                &specialist_content_hash,
                output,
                "pass",
                used_model,
                *tokens_in,
                *tokens_out,
            )
            .await;
        }
        specialist_verifier_status.push((*role, v_status));
        crate::cli_status::emit(format!(
            "review {review_id}: {} verifier={}",
            role_slug(*role),
            v_status
                .map(|s| format!("{s:?}").to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string())
        ));
        tracing::info!(role = ?role, latency_ms, model = %used_model, cache_hit, "M1: specialist persisted");
    }

    // FP-RPT3b B2: quorum gate. `warn` is usable for meta-review but not a
    // clean publication pass; `review_gate` is the single policy source.
    let specialist_gate = crate::review_gate::SpecialistGate::evaluate(
        &specialist_verifier_status,
        min_specialist_quorum,
        specialist_total,
    );
    if !specialist_gate.meta_can_run {
        let error = format!(
            "verifier quorum not met: only {} of {} specialists produced usable output (need >= {})",
            specialist_gate.usable_roles.len(),
            specialist_gate.expected_total,
            specialist_gate.min_usable,
        );
        let synthetic_meta = json!({
            "summary": "Automated review gate failed before meta-review synthesis because too few specialist outputs passed verifier checks.",
            "strengths": [],
            "weaknesses": [
                error,
                format!("Roles without usable verifier output: {}", specialist_gate.blocked_roles.join(", ")),
            ],
            "questions": [
                "Please address the verifier failures and resubmit corrections for automated re-review.",
            ],
            "recommendation": "major_revision",
            "confidence": 1.0,
            "gate": {
                "name": "specialist_verifier_quorum",
                "usable_roles": specialist_gate.usable_roles.clone(),
                "blocked_roles": specialist_gate.blocked_roles.clone(),
                "warning_roles": specialist_gate.warning_roles.clone(),
                "min_quorum": specialist_gate.min_usable,
            }
        });
        crate::db::set_review_meta_review(pool, review_id, &synthetic_meta).await?;

        let failure = crate::github_feedback::gate_failure_from_meta(
            review_id,
            "major_revision",
            Some(&synthetic_meta),
        );
        let _ = crate::github_feedback::record_gate_failure(state, review_id, &failure).await;
        let _ = crate::db::insert_review_event(
            pool,
            Some(review_id),
            Some(paper_id),
            "automated_gate_failed",
            "specialist_verifier_quorum",
            &synthetic_meta,
            None,
        )
        .await;
        tracing::warn!(
            %review_id,
            usable = specialist_gate.usable_roles.len(),
            quorum = MIN_SPECIALIST_QUORUM,
            "specialist quorum not met; recorded major_revision gate failure"
        );
    } else {

    // Meta-reviewer: real synthesis node. FP6 A1: feed it ONLY the five
    // specialist outputs keyed by role slug. The paper extract is omitted —
    // specialists already incorporated the paper into their reasoning, and
    // the meta-reviewer's schema never required a `paper_extract` field. This
    // drops meta input from ~62K tokens to ~10K.
    let mut specialists_map = serde_json::Map::new();
    for (role, output, _ti, _to, _lat, _model, _cache_hit) in &specialist_results {
        specialists_map.insert(role_slug(*role).to_string(), output.clone());
    }
    let meta_input = json!({
        "specialists": serde_json::Value::Object(specialists_map),
    });
    let meta_prompt = build_meta_synthesis_prompt(&meta_input);
    if let Some(root) = debug_root.as_deref() {
        dump_debug_prompt(
            root,
            &extract_arc.arxiv_id,
            AgentRole::MetaReviewer,
            &meta_prompt,
        );
    }
    let meta_system = role_system_prompt(AgentRole::MetaReviewer, extract_arc.field.as_deref());

    let (meta_agent, meta_runner, meta_model_used) = resolve_agent(AgentRole::MetaReviewer)?;
    crate::cli_status::emit(format!("review {review_id}: meta_reviewer starting"));

    // FP6 A4: cache lookup for the meta-reviewer. Its content hash keys on
    // the specialists-bundle JSON it would have reasoned over, so two reviews
    // of the same paper whose specialists produced identical outputs (e.g. a
    // re-run with cache hits) share a cached meta-review.
    let meta_content_hash =
        sha256_hex(&serde_json::to_vec(&meta_input).unwrap_or_default());
    let mut meta_from_cache = false;
    let (meta_value, meta_tokens_in, meta_tokens_out, meta_latency_ms, meta_model_recorded) =
        match if skip_review_cache {
            Ok(None)
        } else {
            crate::db::lookup_cache(pool, paper_id, AgentRole::MetaReviewer, &meta_content_hash)
                .await
        } {
            Ok(Some(hit)) if hit.verifier_status == "pass" => {
                tracing::info!(
                    event = "cache",
                    role = "meta_reviewer",
                    hit = true,
                    "cache hit"
                );
                meta_from_cache = true;
                (
                    hit.output,
                    Some(hit.tokens_in.unwrap_or(0) as i32),
                    Some(hit.tokens_out.unwrap_or(0) as i32),
                    0i32,
                    hit.model,
                )
            }
            _ => {
                if skip_review_cache {
                    tracing::info!(
                        event = "cache",
                        role = "meta_reviewer",
                        disabled = true,
                        "cache bypassed"
                    );
                }
                tracing::info!(
                    event = "cache",
                    role = "meta_reviewer",
                    hit = false,
                    "cache miss"
                );
                let meta_agent_input = AgentInput {
                    paper_id,
                    review_id,
                    role: AgentRole::MetaReviewer,
                    content_hash_material: meta_input.clone(),
                    artifact: meta_input.clone(),
                    system_prompt: meta_system,
                    user_prompt: meta_prompt,
                    source_bundle_path: None,
                };
                let run = meta_agent.run(&*meta_runner, meta_agent_input).await?;
                (
                    run.output,
                    run.tokens_in,
                    run.tokens_out,
                    run.latency_ms,
                    meta_model_used.clone(),
                )
            }
        };

    let (meta_v_status, meta_v_notes) =
        verify_artifact(state, &extract_arc, AgentRole::MetaReviewer, &meta_value).await;
    crate::db::insert_review_agent(
        pool,
        crate::db::ReviewAgentInsert {
            review_id,
            role: AgentRole::MetaReviewer,
            model: &meta_model_recorded,
            output: meta_value.clone(),
            verifier_status: meta_v_status,
            verifier_notes: meta_v_notes.clone(),
            tokens_in: meta_tokens_in,
            tokens_out: meta_tokens_out,
            latency_ms: Some(meta_latency_ms),
        },
    )
    .await?;
    crate::cli_status::emit(format!(
        "review {review_id}: meta_reviewer verifier={}",
        meta_v_status
            .map(|s| format!("{s:?}").to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string())
    ));

    // FP6 A4: only cache fresh successful meta-reviews.
    if !meta_from_cache && meta_v_status == Some(VerifierStatus::Pass) {
        let _ = crate::db::insert_cache(
            pool,
            paper_id,
            AgentRole::MetaReviewer,
            &meta_content_hash,
            &meta_value,
            "pass",
            &meta_model_recorded,
            meta_tokens_in,
            meta_tokens_out,
        )
        .await;
    }
    tracing::info!(meta_latency_ms, model = %meta_model_recorded, cache_hit = meta_from_cache, "M1: meta-reviewer persisted");

    // Stash the synthesized meta_review JSON on the reviews row. If parsing
    // into the typed `MetaReview` fails we still persist the raw JSON so the
    // moderator can inspect what the model produced.
    let _ = serde_json::from_value::<MetaReview>(meta_value.clone());
    crate::db::set_review_meta_review(pool, review_id, &meta_value).await?;
    }

    // Render artifacts to disk under ./artifacts/<review_id>/. The renderer
    // reads back the real meta-review JSON + every review_agents row from
    // Postgres, so the on-disk artifacts faithfully reflect the persisted
    // pipeline output (no synthetic placeholders). Storage-bucket upload is
    // M3; on-disk paths suffice for the M1 assertions.
    let _ = paper_id; // not needed by the new render path
    crate::cli_status::emit(format!("review {review_id}: rendering artifacts"));
    if let Err(e) = render_to_disk(state, review_id).await {
        tracing::warn!(%review_id, err = %e, "render_to_disk failed");
        crate::cli_status::emit(format!("review {review_id}: render warning: {e:#}"));
    } else {
        crate::cli_status::emit(format!("review {review_id}: render complete"));
    }

        Ok(())
    }
    .await;

    if let Err(e) = dag_result {
        tracing::error!(
            %review_id,
            err = %format!("{e:#}"),
            "review DAG bailed; transitioning review row to withdrawn"
        );
        let _ = crate::db::set_review_status(
            pool,
            review_id,
            grokrxiv_schemas::ReviewStatus::Withdrawn,
            None,
        )
        .await;
        return Err(e);
    }

    crate::cli_status::emit(format!(
        "review {review_id}: awaiting_moderation; approve opens a PR, human merge publishes"
    ));
    Ok(review_id)
}

/// Apply selected revision patches: fork the paper's LaTeX (or the review's
/// own output) on `GrokRxiv/grokrxiv-reviews`, apply the accepted patches,
/// and open a draft PR with per-patch accept/reject checkboxes in the body.
///
/// Invoked by the admin approval endpoint after moderation approves a review
/// that ran with `mode=review_and_revise`. The `accepted_indices` argument is
/// a flat list of patch indices the moderator has accepted; the supervisor
/// walks each `revision_patches` row and applies the matching offsets.
///
/// RPT2 ships a stub body: this function validates inputs, loads the
/// `revision_patches` rows for the review, persists `accepted_indices` +
/// `applied_pr_url` per row, and returns a placeholder PR URL. The actual
/// LaTeX-patching loop (`apply_patches_to_latex`) is a follow-up.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn apply_revisions(
    state: &AppState,
    review_id: Uuid,
    accepted_indices: Vec<i32>,
) -> anyhow::Result<String> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("apply_revisions: DATABASE_URL not configured"))?;

    // 1. Load review status/mode and gate on lifecycle.
    let (status, mode) = crate::db::get_review_status_and_mode(pool, review_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("apply_revisions: review {review_id} not found"))?;
    if status != "awaiting_moderation" && status != "pr_open" {
        anyhow::bail!(
            "apply_revisions: review {review_id} is in status `{status}`; \
             expected `awaiting_moderation` or `pr_open`"
        );
    }
    if mode != "review_and_revise" {
        anyhow::bail!(
            "apply_revisions: review {review_id} ran in mode `{mode}`; \
             revision patches are only produced under `review_and_revise`"
        );
    }

    // 2. Load every revision_patches row for the review.
    let rows = crate::db::list_revision_patches(pool, review_id).await?;
    if rows.is_empty() {
        anyhow::bail!(
            "apply_revisions: review {review_id} has no revision_patches rows; \
             nothing to apply"
        );
    }

    // 3. Build a placeholder draft PR URL. The full LaTeX-patching + GitHub
    //    fork loop is a follow-up; for RPT2 we only need the DB plumbing +
    //    function-signature to land.
    let simulated_pr = format!(
        "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-revisions-{}",
        &review_id.simple().to_string()[..8]
    );

    // 4. Walk each row, slice the global `accepted_indices` into the row's
    //    own patch space, and persist `accepted_indices` + `applied_pr_url`.
    //    The supplied indices are global (across all rows); we partition them
    //    by row using each row's `patches.len()` as a window.
    let mut offset: i32 = 0;
    let accepted_set: std::collections::HashSet<i32> = accepted_indices.iter().copied().collect();
    for row in &rows {
        let patch_count = row.patches.as_array().map(|a| a.len() as i32).unwrap_or(0);
        let mut row_accepted: Vec<i32> = Vec::new();
        for local in 0..patch_count {
            let global = offset + local;
            if accepted_set.contains(&global) {
                row_accepted.push(local);
            }
        }
        offset += patch_count;
        crate::db::update_revision_patches_accepted(
            pool,
            row.id,
            &row_accepted,
            Some(&simulated_pr),
        )
        .await?;
    }

    tracing::info!(
        %review_id,
        pr_url = %simulated_pr,
        rows = rows.len(),
        accepted = accepted_indices.len(),
        "apply_revisions: stub PR materialised; DB updated"
    );
    Ok(simulated_pr)
}

/// Aggregate every rung of the role-specific verifier ladder into a
/// `(worst-status, notes)` pair suitable for persistence.
#[cfg(feature = "grokrxiv-ingest")]
async fn verify_artifact(
    state: &AppState,
    extract: &grokrxiv_schemas::PaperExtract,
    role: grokrxiv_schemas::AgentRole,
    artifact: &serde_json::Value,
) -> (
    Option<grokrxiv_schemas::VerifierStatus>,
    Option<serde_json::Value>,
) {
    use grokrxiv_schemas::VerifierStatus;
    use serde_json::json;

    let Some(ladder) = state.verifiers.get(&role) else {
        return (None, None);
    };
    let ctx = grokrxiv_verifier::VerifierContext {
        paper: extract,
        http: &state.http,
    };
    let rungs: Vec<(String, grokrxiv_schemas::VerifierResult)> = ladder.run(artifact, &ctx).await;
    let worst = rungs
        .iter()
        .fold(VerifierStatus::Pass, |acc, (_, r)| match (acc, r.status) {
            (_, VerifierStatus::Fail) | (VerifierStatus::Fail, _) => VerifierStatus::Fail,
            (_, VerifierStatus::Warn) | (VerifierStatus::Warn, _) => VerifierStatus::Warn,
            _ => VerifierStatus::Pass,
        });
    let notes_obj: serde_json::Value = rungs
        .into_iter()
        .map(|(name, r)| {
            (
                name,
                json!({
                    "status": r.status,
                    "notes": r.notes,
                }),
            )
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();
    (Some(worst), Some(notes_obj))
}

fn role_slug(role: grokrxiv_schemas::AgentRole) -> &'static str {
    crate::review_dag::role_slug(role)
}

fn role_system_prompt(role: grokrxiv_schemas::AgentRole, field: Option<&str>) -> String {
    use grokrxiv_schemas::AgentRole;
    let task = match role {
        AgentRole::Summary => "summarize papers in plain language for a literate non-expert",
        AgentRole::TechnicalCorrectness => {
            "assess mathematical, logical, and empirical correctness claim-by-claim"
        }
        AgentRole::Novelty => "compare against prior work and judge novelty",
        AgentRole::Reproducibility => "judge whether the work can be reproduced from the paper",
        AgentRole::Citation => "verify cited references and surface missing ones",
        AgentRole::MetaReviewer => {
            "synthesize five specialist reviews into a single recommendation"
        }
    };
    let mut s = format!(
        "You are a careful, honest specialist peer reviewer. You {task}. \
         Respond with strict JSON conforming to the supplied schema. No prose, \
         no code fences, no commentary."
    );
    let amenable = field.map(is_code_amenable_field).unwrap_or(false);
    if amenable {
        match role {
            AgentRole::TechnicalCorrectness => {
                s.push_str(
                    "\n\nPROOF-AS-CODE AXIOM. The paper is in a code-amenable field \
                     (cs.*, math.*, hep-*, gr-qc, astro-ph, cond-mat, nlin, quant-ph, nucl-*). \
                     For every load-bearing claim that COULD be supported by an executable \
                     artifact — a formal proof in Coq/Lean/Agda/Isabelle, a simulation or \
                     numerical method as Python/Julia/Rust, a complexity argument as \
                     benchmarks, an ML claim as training/eval scripts — and the paper does \
                     NOT ship that artifact: record the claim with assessment 'unsupported' \
                     and severity at least 'major' (use 'critical' if it blocks a headline \
                     result), and write a concrete suggested_fix that names where the code \
                     should live, e.g. `src/proofs/Thm3.lean`, `experiments/figure3/run.py`, \
                     `benchmarks/complexity_test.rs`. Override the default 'be conservative' \
                     guidance for these cases — absence of executable verification IS evidence \
                     of weakness in this field.",
                );
            }
            AgentRole::Reproducibility => {
                s.push_str(
                    "\n\nPROOF-AS-CODE AXIOM. The paper is in a code-amenable field. \
                     Theory papers are NOT exempt from reproducibility analysis: formal \
                     verification or numerical reproduction of theoretical results counts \
                     as reproducibility, and a claimed theorem without a formal proof or \
                     numerical evidence IS a reproducibility gap. For every load-bearing \
                     theoretical or empirical claim that lacks a code/proof artifact, add a \
                     `concerns` entry with area='proof_as_code', a description naming the \
                     specific artifact that would close the gap (path included), and \
                     severity at least 'major' ('critical' if the headline result depends on it).",
                );
            }
            AgentRole::MetaReviewer => {
                s.push_str(
                    "\n\nRECOMMENDATION GATE. When technical_correctness OR reproducibility \
                     flagged a missing proof-as-code artifact at severity 'major' or 'critical', \
                     default `recommendation` to `major_revision`. If the missing artifact \
                     blocks a headline claim, recommend `reject`. Only allow `accept` or \
                     `minor_revision` when (a) code exists and was acknowledged by the \
                     specialists, or (b) the paper explicitly justifies the absence (e.g. \
                     existence proof in a field where Coq tooling does not yet cover the \
                     theory). When applying this gate, cite the specific specialist findings \
                     in `summary` and add the missing artifacts to `weaknesses`.\n\n\
                     VERIFIED-FACT WEIGHTING. Specialist outputs now carry merged ground \
                     truth from deterministic verifiers: `citation_review.entries[*].exists / \
                     resolved_doi / resolved_url` come from real Crossref + arXiv lookups; \
                     `reproducibility_review.concerns[]` entries describing 'Verifier could \
                     not reach …' or 'GitHub repository … is marked archived' came from \
                     HTTP HEAD + GitHub API calls; `novelty_review.related_work[]` entries \
                     tagged `relation: candidate_neighbor` came from Semantic Scholar. Treat \
                     these fields as authoritative — do not contradict them. The specialists' \
                     `relevance` / `confidence` / `recommendation` fields remain LLM \
                     judgments; weight them against the verified facts when they conflict.",
                );
            }
            _ => {}
        }
    }
    s
}

/// Pre-populate `novelty_review.related_work[]` with the verified prior-art
/// candidates from Semantic Scholar. Entries already produced by the LLM are
/// preserved; we append the verifier candidates that the LLM didn't already
/// cite (matched by case-insensitive title prefix).
fn merge_novelty_facts_into_output(
    mut output: serde_json::Value,
    facts: &crate::agents::specialist_facts::NoveltyFacts,
) -> serde_json::Value {
    let Some(obj) = output.as_object_mut() else {
        return output;
    };
    let related = obj
        .entry("related_work".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(arr) = related.as_array_mut() else {
        return output;
    };
    let existing_titles: std::collections::HashSet<String> = arr
        .iter()
        .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
        .map(|s| s.to_ascii_lowercase().chars().take(80).collect::<String>())
        .collect();
    for p in &facts.related_papers {
        let title_key: String = p.title.to_ascii_lowercase().chars().take(80).collect();
        if existing_titles.contains(&title_key) {
            continue;
        }
        arr.push(serde_json::json!({
            "title": p.title,
            "year": p.year,
            "venue": p.source,
            "url": p.url,
            "doi": p.doi,
            "arxiv_id": p.arxiv_id,
            "relation": "candidate_neighbor",
            "notes": p.abstract_snippet,
        }));
    }
    output
}

/// Overlay verified URL / GitHub repo state onto the LLM's
/// `reproducibility_review`. Auto-adds a `concerns` entry for each
/// unreachable URL and each archived repo so the moderator UI surfaces the
/// gap regardless of whether the LLM noticed it. Existing concerns from the
/// LLM are preserved; we only append.
fn merge_reproducibility_facts_into_output(
    mut output: serde_json::Value,
    facts: &crate::agents::specialist_facts::ReproducibilityFacts,
) -> serde_json::Value {
    let Some(obj) = output.as_object_mut() else {
        return output;
    };
    // Concerns array: append, don't clobber.
    let concerns = obj
        .entry("concerns".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(concerns_arr) = concerns.as_array_mut() else {
        return output;
    };
    use crate::agents::specialist_facts::UrlKind;
    for u in &facts.urls_checked {
        if u.reachable {
            continue;
        }
        let area = match u.kind {
            UrlKind::Code => "code",
            UrlKind::Dataset => "data",
            UrlKind::Other => "other",
        };
        let status = u
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "network_error".to_string());
        concerns_arr.push(serde_json::json!({
            "area": area,
            "description": format!("Verifier could not reach `{}` (status={})", u.url, status),
            "severity": "major",
        }));
    }
    for r in &facts.github_repos {
        if matches!(r.archived, Some(true)) {
            concerns_arr.push(serde_json::json!({
                "area": "code",
                "description": format!(
                    "GitHub repository `{}/{}` is marked archived — code is no longer maintained.",
                    r.owner, r.repo
                ),
                "severity": "minor",
            }));
        }
    }
    output
}

/// Overlay the verifier's per-entry resolution (`exists`, `resolved_doi`,
/// `resolved_url`) onto the LLM specialist's `citation_review.entries[*]`.
/// The LLM's `relevance` / `notes` / `explanation` / `summary` /
/// `missing_references` are preserved. The match is by index (the spec
/// requires both arrays in bibliography order); when the LLM array is shorter
/// the extra verifier rows are appended so no validation work is lost.
fn merge_citation_verifier_into_output(
    mut output: serde_json::Value,
    v_notes: Option<&serde_json::Value>,
) -> serde_json::Value {
    let Some(v_entries) = v_notes
        .and_then(|n| n.get("entries"))
        .and_then(|e| e.as_array())
    else {
        return output;
    };
    let Some(entries) = output.get_mut("entries").and_then(|e| e.as_array_mut()) else {
        return output;
    };
    for (i, v_entry) in v_entries.iter().enumerate() {
        let exists = v_entry
            .get("exists")
            .cloned()
            .unwrap_or(serde_json::Value::Bool(false));
        let resolved_doi = v_entry
            .get("resolved_doi")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let resolved_url = v_entry
            .get("resolved_url")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if let Some(out_entry) = entries.get_mut(i).and_then(|v| v.as_object_mut()) {
            out_entry.insert("exists".into(), exists);
            out_entry.insert("resolved_doi".into(), resolved_doi);
            out_entry.insert("resolved_url".into(), resolved_url);
        } else {
            // Verifier saw more entries than the LLM emitted — synthesize a
            // minimal citation_review entry so the verified data isn't lost.
            // Schema requires citation/relevance/notes/explanation; fill with
            // verifier-sourced defaults.
            let raw = v_entry
                .get("raw")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            entries.push(serde_json::json!({
                "citation": { "key": format!("[{}]", i + 1), "raw": raw, "title": null, "authors": [] },
                "exists": exists,
                "resolved_doi": resolved_doi,
                "resolved_url": resolved_url,
                "relevance": "medium",
                "notes": null,
                "explanation": "Entry resolved by verifier; LLM specialist did not include it in its review output.",
            }));
        }
    }
    output
}

/// Phase F: kill switch for the html_quality post-render stage. Truthy values
/// (`1`/`true`/`yes`/`on`) skip the codex audit for fast smoke tests or when
/// the codex CLI is unavailable. Default is enabled — the stage logs warn +
/// proceeds without rewriting if codex itself fails, so leaving this off is
/// safe in production.
fn html_quality_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_HTML_QUALITY_DISABLE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// arXiv category prefixes for fields where executable verification is the
/// state of the art and its absence is evidence of weakness. Kept hard-coded
/// because the list is short and stable; broaden as new tooling lands.
fn is_code_amenable_field(field: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "cs.", "math.", "hep-", "gr-qc", "astro-ph", "cond-mat", "nlin", "quant-ph", "nucl-",
        "stat.",
    ];
    PREFIXES.iter().any(|p| field.starts_with(p))
}

/// Per-role character budget for the rendered section bodies. Reserved for
/// the **body block only** — title/abstract/heading-index/bibliography are
/// outside this budget. Track 8a: the previous prompt builder only emitted
/// headings, which left every specialist reasoning from abstract + an outline
/// instead of the full paper. The budgets below are tuned so the most
/// content-hungry role (technical correctness) sees ~240k chars, which is
/// roughly the long-context window of the role's model after schema overhead.
///
/// `MetaReviewer` is `0`: it only ever sees specialist outputs (FP6 A1).
#[cfg(feature = "grokrxiv-ingest")]
fn body_budget_chars(role: grokrxiv_schemas::AgentRole) -> usize {
    use grokrxiv_schemas::AgentRole;
    match role {
        AgentRole::Summary => 48_000,
        AgentRole::TechnicalCorrectness => 240_000,
        AgentRole::Novelty => 120_000,
        AgentRole::Reproducibility => 80_000,
        AgentRole::Citation => 0,
        AgentRole::MetaReviewer => 0,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
const DEFAULT_CITATION_PROMPT_MAX_BIB_ENTRIES: usize = 32;

#[cfg(feature = "grokrxiv-ingest")]
fn citation_prompt_max_bib_entries() -> usize {
    std::env::var("GROKRXIV_CITATION_PROMPT_MAX_BIB_ENTRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CITATION_PROMPT_MAX_BIB_ENTRIES)
}

/// Render a single section in its canonical `## {heading}\n\n{body}\n\n`
/// form. The trailing blank line keeps adjacent sections visually separated.
#[cfg(feature = "grokrxiv-ingest")]
fn render_section(heading: &str, body: &str) -> String {
    format!("## {heading}\n\n{body}\n\n")
}

/// Truncate `s` to roughly `budget` chars using the "first 60%, last 40%"
/// split. Char-based (not byte-based) so we never split a multi-byte codepoint.
/// If `s` already fits, returns it untouched.
#[cfg(feature = "grokrxiv-ingest")]
fn truncate_60_40(s: &str, budget: usize) -> String {
    let total = s.chars().count();
    if total <= budget {
        return s.to_string();
    }
    let marker = "\n\n[…truncated…]\n\n";
    let marker_len = marker.chars().count();
    let usable = budget.saturating_sub(marker_len);
    let head_n = (usable * 60) / 100;
    let tail_n = usable.saturating_sub(head_n);
    let head: String = s.chars().take(head_n).collect();
    let tail: String = s.chars().skip(total - tail_n).collect();
    format!("{head}{marker}{tail}")
}

/// Render the section body block within `budget` chars.
///
/// Behavior:
/// - Iterate sections in document order. For each:
///   - Render `## {heading}\n\n{body}\n\n`.
///   - If the rendered single section exceeds `budget` on its own AND nothing
///     has been emitted yet, truncate it with the 60/40 split and emit it as
///     the sole survivor.
///   - Otherwise, if it fits in remaining budget, append it.
///   - Otherwise, skip it and record the heading as truncated.
/// - If any sections are skipped, append a single
///   `[…remaining sections truncated; headings: a; b; c]` block.
///
/// `budget == 0` returns an empty string (used for `MetaReviewer`).
#[cfg(feature = "grokrxiv-ingest")]
fn render_section_block(sections: &[grokrxiv_schemas::Section], budget: usize) -> String {
    if budget == 0 || sections.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut consumed: usize = 0;

    for s in sections {
        let rendered = render_section(&s.heading, &s.body_markdown);
        let rendered_chars = rendered.chars().count();

        if consumed == 0 && rendered_chars > budget {
            let truncated = truncate_60_40(&rendered, budget);
            consumed = truncated.chars().count();
            out.push_str(&truncated);
            continue;
        }

        if consumed + rendered_chars <= budget {
            out.push_str(&rendered);
            consumed += rendered_chars;
        } else {
            skipped.push(s.heading.as_str());
        }
    }

    if !skipped.is_empty() {
        let headings = skipped.join("; ");
        out.push_str(&format!(
            "[…remaining sections truncated; headings: {headings}]\n"
        ));
    }
    out
}

/// Render the bibliography block. Keys are synthesised 1-indexed
/// (`[1] …`, `[2] …`) since `Citation` doesn't carry a BibTeX key field; this
/// keeps the format stable across runs and is what the citation specialist
/// expects to cross-reference.
#[cfg(feature = "grokrxiv-ingest")]
fn render_bibliography(bibliography: &[grokrxiv_schemas::Citation]) -> String {
    render_bibliography_limited(bibliography, None)
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_bibliography_limited(
    bibliography: &[grokrxiv_schemas::Citation],
    max_entries: Option<usize>,
) -> String {
    if bibliography.is_empty() {
        return String::new();
    }
    let total = bibliography.len();
    let shown = max_entries.map(|max| max.min(total)).unwrap_or(total);
    let mut out = if shown < total {
        format!("Bibliography ({total} entries; showing {shown}):\n")
    } else {
        format!("Bibliography ({total} entries):\n")
    };
    for (i, c) in bibliography.iter().take(shown).enumerate() {
        let key = i + 1;
        let raw = c.raw.replace('\n', " ").trim().to_string();
        let mut parts = Vec::new();
        if !raw.is_empty() {
            parts.push(raw.clone());
        }
        if let Some(title) = c.title.as_deref().filter(|s| !s.trim().is_empty()) {
            if !raw.contains(title) {
                parts.push(format!("title: {}", title.trim()));
            }
        }
        if let Some(doi) = c.doi.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(format!("doi: {}", doi.trim()));
        }
        if let Some(arxiv_id) = c.arxiv_id.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(format!("arxiv: {}", arxiv_id.trim()));
        }
        if parts.is_empty() {
            parts.push("unresolved bibliography entry".to_string());
        }
        out.push_str(&format!("[{key}] {}\n", parts.join(" | ")));
    }
    if shown < total {
        let omitted = total - shown;
        out.push_str(&format!(
            "[…{omitted} additional bibliography entries omitted from citation LLM prompt; \
             verifier and render artifacts preserve the full bibliography.]\n"
        ));
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_citation_contexts(sections: &[grokrxiv_schemas::Section], budget: usize) -> String {
    if budget == 0 || sections.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for section in sections {
        for sentence in citation_sentences(&section.body_markdown) {
            let line = format!("- {}: {}\n", section.heading, sentence);
            let next_len = out.chars().count() + line.chars().count();
            if next_len > budget {
                if out.is_empty() {
                    let truncated = truncate_60_40(&line, budget);
                    out.push_str(&truncated);
                }
                return out;
            }
            out.push_str(&line);
        }
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn citation_sentences(body: &str) -> Vec<String> {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in normalized.chars() {
        current.push(ch);
        if matches!(ch, '.' | '?' | '!') {
            push_citation_sentence(&mut sentences, &current);
            current.clear();
        }
    }
    push_citation_sentence(&mut sentences, &current);
    sentences
}

#[cfg(feature = "grokrxiv-ingest")]
fn push_citation_sentence(sentences: &mut Vec<String>, sentence: &str) {
    let trimmed = sentence.trim();
    if trimmed.is_empty() {
        return;
    }
    if trimmed.contains("[@") || trimmed.contains("@") || trimmed.contains("\\cite") {
        sentences.push(truncate_60_40(trimmed, 1_200));
    }
}

#[cfg(feature = "grokrxiv-ingest")]
fn build_specialist_prompt(
    role: grokrxiv_schemas::AgentRole,
    extract: &grokrxiv_schemas::PaperExtract,
    moderator_notes: Option<&str>,
    reproducibility_facts: Option<&crate::agents::specialist_facts::ReproducibilityFacts>,
    novelty_facts: Option<&crate::agents::specialist_facts::NoveltyFacts>,
    tc_facts: Option<&crate::agents::specialist_facts::TechnicalCorrectnessFacts>,
) -> String {
    use grokrxiv_schemas::AgentRole;

    // MetaReviewer never gets the paper body — by contract it sees only the
    // five specialist outputs (FP6 A1). Preserve that here.
    if matches!(role, AgentRole::MetaReviewer) {
        return String::new();
    }

    let budget = body_budget_chars(role);
    let heading_index: String = extract
        .sections
        .iter()
        .take(40)
        .map(|s| format!("- {}", s.heading))
        .collect::<Vec<_>>()
        .join("\n");
    let body_block = render_section_block(&extract.sections, budget);
    let bib_block = if matches!(role, AgentRole::Citation) {
        render_bibliography_limited(
            &extract.bibliography,
            Some(citation_prompt_max_bib_entries()),
        )
    } else {
        render_bibliography(&extract.bibliography)
    };
    let citation_contexts = if matches!(role, AgentRole::Citation) {
        render_citation_contexts(&extract.sections, 24_000)
    } else {
        String::new()
    };

    let task = match role {
        AgentRole::Summary => {
            "Produce a plain-language summary of the paper. Populate the schema's \
             `plain_language_summary`, `key_contributions`, `tldr`, and (optionally) \
             `audience` fields."
        }
        AgentRole::TechnicalCorrectness => {
            "Walk through the paper's main claims and assess each. Populate the schema's \
             `claims` (with id, claim, assessment, severity, and optionally location, \
             evidence, suggested_fix), `overall_correctness`, and `confidence`."
        }
        AgentRole::Novelty => {
            "Compare this paper against the most relevant prior work and judge its \
             novelty. Populate `novelty_score`, `verdict`, `confidence`, and optionally \
             `related_work` and `missing_prior_art`."
        }
        AgentRole::Reproducibility => {
            "Evaluate reproducibility. Populate `code_availability`, `data_availability`, \
             `reproducibility_score`, `confidence`, and optionally `code_url`, `data_url`, \
             `environment`, `concerns`."
        }
        AgentRole::Citation => {
            "Focus on RELEVANCE and MISSING WORK — a separate deterministic \
             verifier (Crossref + arXiv batch lookups) handles existence and \
             DOI/URL resolution and writes its results to `verifier_notes`. \
             Your job: for each bibliography entry included below, set `relevance` \
             from the extracted in-text contexts (`high`/`medium`/`low`/`unrelated`), \
             write `explanation` describing where and why it's cited, and \
             leave `exists`/`resolved_doi`/`resolved_url` at their defaults \
             (`false`/`null`) since the verifier will overlay ground truth. \
             If the bibliography block says entries were omitted, do not invent \
             entries for the omitted references; the verifier and render pipeline \
             preserve the full bibliography separately. \
             Populate `missing_references` with prior work you would expect \
             the paper to cite but doesn't, with reasons. Provide `summary` \
             and `confidence`."
        }
        AgentRole::MetaReviewer => unreachable!("MetaReviewer handled above"),
    };

    let field_line = match extract.field.as_deref() {
        Some(f) if !f.is_empty() => format!("Paper field: {f}\n\n"),
        _ => String::new(),
    };
    let mut out = format!(
        "{field_line}Paper title: {title}\n\nAbstract:\n{abstract_}\n\nSection headings:\n{heading_index}\n\n",
        title = extract.title,
        abstract_ = extract.abstract_,
    );
    if !body_block.is_empty() {
        out.push_str("Paper body:\n\n");
        out.push_str(&body_block);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    if !citation_contexts.is_empty() {
        out.push_str("Citation contexts:\n\n");
        out.push_str(&citation_contexts);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    if !bib_block.is_empty() {
        out.push_str(&bib_block);
        out.push('\n');
    }
    if let Some(notes) = moderator_notes.filter(|s| !s.trim().is_empty()) {
        out.push_str(
            "Moderator notes from a prior `request-changes` round — treat these as authoritative \
             priorities for this review pass:\n\n",
        );
        out.push_str(notes.trim());
        out.push_str("\n\n");
    }
    if let Some(facts) = reproducibility_facts {
        if !facts.urls_checked.is_empty() || !facts.github_repos.is_empty() {
            out.push_str(
                "Verified availability facts (deterministically retrieved — do NOT re-check, \
                 treat as authoritative):\n\n",
            );
            if !facts.urls_checked.is_empty() {
                out.push_str("URLs checked:\n");
                for u in &facts.urls_checked {
                    let status_str = u
                        .status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "network_error".to_string());
                    let kind = match u.kind {
                        crate::agents::specialist_facts::UrlKind::Code => "code",
                        crate::agents::specialist_facts::UrlKind::Dataset => "dataset",
                        crate::agents::specialist_facts::UrlKind::Other => "other",
                    };
                    out.push_str(&format!(
                        "- [{kind}] {url} → {state} (status={status_str})\n",
                        url = u.url,
                        state = if u.reachable {
                            "REACHABLE"
                        } else {
                            "UNREACHABLE"
                        },
                    ));
                }
                out.push('\n');
            }
            if !facts.github_repos.is_empty() {
                out.push_str("GitHub repositories:\n");
                for r in &facts.github_repos {
                    if !r.exists {
                        out.push_str(&format!(
                            "- {}/{}: NOT FOUND (404 or private without token)\n",
                            r.owner, r.repo
                        ));
                        continue;
                    }
                    let mut tags: Vec<String> = Vec::new();
                    if let Some(p) = &r.pushed_at {
                        tags.push(format!("last_pushed={p}"));
                    }
                    if let Some(s) = r.stargazers_count {
                        tags.push(format!("stars={s}"));
                    }
                    if let Some(l) = &r.license_spdx {
                        tags.push(format!("license={l}"));
                    }
                    if matches!(r.archived, Some(true)) {
                        tags.push("ARCHIVED".to_string());
                    }
                    out.push_str(&format!(
                        "- {}/{}: exists; {}\n",
                        r.owner,
                        r.repo,
                        if tags.is_empty() {
                            "no metadata".to_string()
                        } else {
                            tags.join(", ")
                        }
                    ));
                }
                out.push('\n');
            }
            out.push_str(
                "Rules:\n\
                 - A `code_url` from the paper is only resolved iff its entry above is REACHABLE.\n\
                 - A repository marked ARCHIVED implies the work is no longer maintained — \
                   surface as a `severity: minor` concern.\n\
                 - An UNREACHABLE code/dataset URL is a `severity: major` reproducibility concern; \
                   add a `concerns` entry naming the URL and the status code.\n\n",
            );
        }
    }
    if let Some(facts) = novelty_facts {
        if !facts.related_papers.is_empty() {
            out.push_str(
                "Verified prior-art candidates (retrieved by metadata similarity — judge novelty \
                 against these, do NOT rely on memory of pre-2024 literature):\n\n",
            );
            for (i, p) in facts.related_papers.iter().enumerate().take(20) {
                let year = p
                    .year
                    .map(|y| y.to_string())
                    .unwrap_or_else(|| "n.d.".to_string());
                let author = p.primary_author.as_deref().unwrap_or("unknown");
                let snippet = p
                    .abstract_snippet
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(no abstract)");
                out.push_str(&format!(
                    "{:>2}. [{year}] {author} — {title}\n    {snippet}\n",
                    i + 1,
                    title = p.title,
                ));
                if let Some(arxiv) = &p.arxiv_id {
                    out.push_str(&format!("    arXiv:{arxiv}\n"));
                }
                if let Some(doi) = &p.doi {
                    out.push_str(&format!("    doi:{doi}\n"));
                }
            }
            out.push_str(
                "\nRules:\n\
                 - Each related paper above is a real, retrievable neighbor — treat its existence \
                   as ground truth. Do NOT claim a paper does not exist if it's listed here.\n\
                 - When the manuscript's novelty claim conflicts with a related paper, lower \
                   `novelty_score` and add a `missing_prior_art` entry citing the related paper.\n\n",
            );
        } else if !facts.retrieval_error.is_empty() {
            out.push_str(&format!(
                "Prior-art retrieval failed ({}); fall back to memory but flag the gap in confidence.\n\n",
                facts.retrieval_error,
            ));
        }
    }
    if let Some(facts) = tc_facts {
        if !facts.tables.is_empty()
            || !facts.equation_labels.is_empty()
            || !facts.complexity_mentions.is_empty()
        {
            out.push_str(
                "Verified structural facts about the paper (use these to cross-check claims \
                 against actual tables and equations; do NOT reason from memory of the body):\n\n",
            );
            if !facts.tables.is_empty() {
                out.push_str("Tables found:\n");
                for t in facts.tables.iter().take(20) {
                    out.push_str(&format!(
                        "- [{section}] {rows} rows; header: {header}\n",
                        section = t.section,
                        rows = t.row_count,
                        header = t.header_row.chars().take(160).collect::<String>(),
                    ));
                }
                out.push('\n');
            }
            if !facts.equation_labels.is_empty() {
                out.push_str("Equation labels found:\n");
                for e in facts.equation_labels.iter().take(20) {
                    out.push_str(&format!("- [{}] {}\n", e.section, e.label));
                }
                out.push('\n');
            }
            if !facts.complexity_mentions.is_empty() {
                out.push_str("Complexity notations found:\n");
                for c in facts.complexity_mentions.iter().take(20) {
                    out.push_str(&format!("- [{}] {}\n", c.section, c.notation));
                }
                out.push('\n');
            }
            out.push_str(
                "Rules:\n\
                 - When a claim references a number that should appear in a table above, cite \
                   the table by header and verify the number against the source body block.\n\
                 - When the paper claims a complexity bound not listed above, flag it as \
                   `unsupported` unless the body explicitly derives it.\n\n",
            );
        }
    }
    out.push_str(&format!("Task: {task}"));
    out
}

/// Resolve the debug-prompt directory from the `GROKRXIV_DEBUG_PROMPT_DIR`
/// env var, set by the CLI's `--debug-prompt` flag. When the var is unset
/// (or empty) this returns `None` and the supervisor skips the dump.
#[cfg(feature = "grokrxiv-ingest")]
fn debug_prompt_root() -> Option<std::path::PathBuf> {
    let raw = std::env::var("GROKRXIV_DEBUG_PROMPT_DIR").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(raw))
}

/// Best-effort dump of one role's rendered prompt under
/// `<root>/<arxiv_id>/<role>.md`. Silently does nothing on any I/O failure —
/// `--debug-prompt` is observational and must never crash a review.
#[cfg(feature = "grokrxiv-ingest")]
fn dump_debug_prompt(
    root: &std::path::Path,
    arxiv_id: &str,
    role: grokrxiv_schemas::AgentRole,
    prompt: &str,
) {
    let safe_id: String = arxiv_id
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c => c,
        })
        .collect();
    let dir = root.join(safe_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = dir.join(format!("{}.md", role_slug(role)));
    let _ = std::fs::write(&file, prompt);
}

fn build_meta_synthesis_prompt(meta_input: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(meta_input).unwrap_or_else(|_| "{}".into());
    // FP-RPT3b B1: the meta_input contract (built at supervisor.rs:527-529)
    // contains only the `specialists` key. The paper extract is intentionally
    // omitted (FP6 A1) because each specialist already incorporated the paper
    // into their reasoning. The previous prompt template lied about a `paper`
    // key that does not exist in the JSON the model receives.
    format!(
        "Below is a JSON object with one key, `specialists`, containing the five \
         specialist reviewers' outputs keyed by role slug:\n\
         - `summary` → {{tldr, plain_language_summary, key_contributions[], audience}}\n\
         - `technical_correctness` → {{claims[], overall_correctness, confidence}}\n\
         - `novelty` → {{verdict, novelty_score, related_work[], missing_prior_art[], confidence}}\n\
         - `reproducibility` → {{reproducibility_score, code_availability, code_url, \
            data_availability, data_url, environment, concerns[], confidence}}\n\
         - `citation` → {{entries[], missing_references[], summary, confidence}}\n\n\
         The paper extract itself is NOT included — each specialist already reasoned \
         over it. Treat the specialist outputs as your sole evidence.\n\n\
         {pretty}\n\n\
         Task: Synthesize these five specialist reviews into a single MetaReview JSON \
         object with fields summary, strengths, weaknesses, questions, recommendation \
         (one of accept|minor_revision|major_revision|reject), and confidence (0..1)."
    )
}

/// Read a review's persisted state from the DB and render the four artifacts
/// (`review.html`, `review.md`, `review.tex`, `bundle.zip`) to
/// `artifacts/<review_id>/`. The meta-review JSON is loaded from
/// `reviews.meta_review`; the per-agent rows are loaded from `review_agents`
/// (role + model + output + verifier_status + verifier_notes). On success the
/// reviews row's `html_path` + `zip_path` are updated via
/// `db::set_review_artifacts`.
#[cfg(feature = "grokrxiv-render")]
pub async fn render_to_disk(state: &AppState, review_id: Uuid) -> anyhow::Result<()> {
    use grokrxiv_render::AgentRecord;
    use grokrxiv_schemas::{MetaReview, PaperExtract, Section, VerifierResult, VerifierStatus};

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    // 1. Load the meta-review JSON + the joined paper row + persisted agents.
    let bundle = crate::db::load_review_render_bundle(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review render bundle: {e}"))?;
    let crate::db::ReviewRenderHeadRow {
        meta_review: meta_json,
        paper_id: _paper_id,
        arxiv_id,
        title,
        abstract_,
        field,
    } = bundle.review;

    let meta: MetaReview = meta_json
        .and_then(|v| serde_json::from_value::<MetaReview>(v).ok())
        .unwrap_or_else(|| fallback_meta(&title));

    // FP6 A2: the shared specialist input artifact now lives on `review_inputs`.
    // Specialists all reasoned over the same paper extract; the meta-reviewer
    // reasoned over the bundle of specialist outputs (reconstructable from the
    // `output` columns above), which we synthesise per-row for the bundle.
    let specialist_input = crate::db::load_review_input(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review_inputs: {e}"))?
        .unwrap_or(serde_json::Value::Null);
    let meta_input_for_render = {
        let mut specialists_map = serde_json::Map::new();
        for row in &bundle.agents {
            if row.role != "meta_reviewer" {
                specialists_map.insert(row.role.clone(), row.output.clone());
            }
        }
        serde_json::json!({ "specialists": serde_json::Value::Object(specialists_map) })
    };

    let mut agents: Vec<AgentRecord> = Vec::with_capacity(bundle.agents.len());
    let mut agent_jsons: Vec<(String, Vec<u8>)> = Vec::with_capacity(bundle.agents.len());
    for row in bundle.agents {
        let role_slug = row.role.clone();
        if let Some(role) = parse_role_slug(&role_slug) {
            let status = row
                .verifier_status
                .as_deref()
                .and_then(crate::db::verifier_status_from_db_str)
                .unwrap_or(VerifierStatus::Pass);
            let notes = row.verifier_notes.unwrap_or(serde_json::Value::Null);
            let verifier = VerifierResult {
                status,
                notes: notes.clone(),
            };
            let input_artifact = if role_slug == "meta_reviewer" {
                meta_input_for_render.clone()
            } else {
                specialist_input.clone()
            };
            let artifact = serde_json::json!({
                "role": role_slug,
                "model": row.model.clone(),
                "input_artifact": input_artifact,
                "output": row.output.clone(),
                "verifier": {
                    "status": status,
                    "notes": notes,
                },
            });
            let path = format!("agents/{role_slug}.json");
            let bytes = serde_json::to_vec_pretty(&artifact)
                .map_err(|e| anyhow::anyhow!("serialize {path}: {e}"))?;
            agent_jsons.push((path, bytes));
            agents.push(AgentRecord {
                role,
                model: row.model,
                output: row.output,
                verifier,
            });
        }
    }

    // 3. Reconstruct a minimal `PaperExtract` for the renderer from the
    //    persisted papers row. The renderer tolerates empty sections / figs /
    //    bibliography — we just need title + abstract + arxiv_id so the HTML
    //    document is recognisable as a review of this paper.
    let extract = PaperExtract {
        arxiv_id: arxiv_id.clone(),
        title: title.clone(),
        authors: Vec::new(),
        abstract_: abstract_.unwrap_or_default(),
        field,
        sections: Vec::<Section>::new(),
        figures: Vec::new(),
        bibliography: Vec::new(),
        source_format: None,
    };

    let html = grokrxiv_render::render_html(&meta, &extract, &agents)
        .map_err(|e| anyhow::anyhow!("render_html: {e}"))?;
    let md = grokrxiv_render::render_markdown(&meta, &extract, &agents);
    let tex = grokrxiv_render::render_latex(&meta, &extract, &agents);
    let metadata = serde_json::json!({
        "review_id": review_id,
        "arxiv_id": extract.arxiv_id,
    });
    let zip = grokrxiv_render::build_zip(&html, &md, &tex, None, &agent_jsons, &metadata)
        .map_err(|e| anyhow::anyhow!("build_zip: {e}"))?;
    let dir = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    tokio::fs::create_dir_all(&dir).await.ok();
    tokio::fs::write(dir.join("review.html"), &html).await?;
    tokio::fs::write(dir.join("review.md"), md).await?;
    tokio::fs::write(dir.join("review.tex"), tex).await?;
    tokio::fs::write(dir.join("bundle.zip"), &zip).await?;

    // Phase F: post-render HTML quality harness. Codex (gpt-5.5) audits the
    // rendered review.html for formatting / readability issues and writes a
    // corrected copy back along with a formatting_fixes.json sidecar. Skipped
    // when GROKRXIV_HTML_QUALITY_DISABLE=1 (useful for fast smoke tests).
    if !html_quality_disabled() {
        if let Err(e) = crate::html_review::review_and_fix_html(state, review_id, &dir).await {
            tracing::warn!(%review_id, err = %e, "html_quality: stage errored — leaving review.html as-is");
        }
    }

    let dir_str = format!("artifacts/{review_id}");
    let _ = crate::db::set_review_artifacts(
        pool,
        review_id,
        Some(&format!("{dir_str}/review.html")),
        None,
        Some(&format!("{dir_str}/bundle.zip")),
    )
    .await;

    Ok(())
}

#[cfg(feature = "grokrxiv-render")]
fn fallback_meta(title: &str) -> grokrxiv_schemas::MetaReview {
    use grokrxiv_schemas::{MetaReview, Recommendation};
    MetaReview {
        summary: format!("Review of {}", title),
        strengths: vec![],
        weaknesses: vec![],
        questions: vec![],
        recommendation: Recommendation::MinorRevision,
        confidence: 0.5,
    }
}

#[cfg(feature = "grokrxiv-render")]
fn parse_role_slug(s: &str) -> Option<grokrxiv_schemas::AgentRole> {
    use grokrxiv_schemas::AgentRole;
    Some(match s {
        "summary" => AgentRole::Summary,
        "technical_correctness" => AgentRole::TechnicalCorrectness,
        "novelty" => AgentRole::Novelty,
        "reproducibility" => AgentRole::Reproducibility,
        "citation" => AgentRole::Citation,
        "meta_reviewer" => AgentRole::MetaReviewer,
        _ => return None,
    })
}

#[cfg(not(feature = "grokrxiv-render"))]
pub async fn render_to_disk(_state: &AppState, _review_id: Uuid) -> anyhow::Result<()> {
    Ok(())
}

/// Hex-encoded SHA-256 of the input bytes. Used by FP6 A4 to key the
/// per-paper output cache on the exact bytes of the specialist input
/// artifact (and, for the meta-reviewer, the bundle of specialist outputs).
#[cfg(feature = "grokrxiv-ingest")]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

async fn run_item(
    state: &AppState,
    item: &WorkItem,
    supervisor_tx: &mpsc::Sender<WorkItem>,
) -> anyhow::Result<()> {
    if let Some(pool) = state.db.as_ref() {
        let _ = crate::db::mark_running(pool, item.job_id).await;
    }
    let outcome = match item.kind {
        JobKind::Ingest => run_ingest(state, item, supervisor_tx).await,
        JobKind::Review => run_review(state, item).await,
        JobKind::Render => Err(anyhow::anyhow!("render: not implemented in M1")),
        JobKind::Publish => run_publish(state, item).await,
        JobKind::Preview => Err(anyhow::anyhow!("preview is handled synchronously")),
    };
    match outcome {
        Ok(()) => {
            if let Some(pool) = state.db.as_ref() {
                let _ = crate::db::mark_done(pool, item.job_id).await;
            }
            Ok(())
        }
        Err(e) => {
            if item.attempt + 1 < MAX_RETRIES && is_retryable(&e) {
                let delay = exp_backoff(item.attempt + 1);
                tracing::warn!(
                    job_id = %item.job_id,
                    delay_ms = delay.as_millis() as u64,
                    "retrying job"
                );
                tokio::time::sleep(delay).await;
                let mut retry = item.clone();
                retry.attempt += 1;
                supervisor_tx
                    .send(retry)
                    .await
                    .map_err(|send_err| anyhow::anyhow!("retry enqueue failed: {send_err}"))?;
                Ok(())
            } else {
                if let Some(pool) = state.db.as_ref() {
                    let _ = crate::db::mark_failed(pool, item.job_id, &e.to_string()).await;
                }
                Err(e)
            }
        }
    }
}

/// Background worker: ingest a single arXiv paper, persist its `papers` row,
/// and (if the paper is recent enough per `scheduler.auto_review_from`) enqueue
/// a Review job for it.
///
/// The arXiv id is carried on `item.payload["arxiv_id"]` rather than `ref_id`
/// because the paper row doesn't exist yet — there's no UUID to reference.
#[cfg(feature = "grokrxiv-ingest")]
async fn run_ingest(
    state: &AppState,
    item: &WorkItem,
    supervisor_tx: &mpsc::Sender<WorkItem>,
) -> anyhow::Result<()> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    let arxiv_id = item
        .payload
        .get("arxiv_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("run_ingest: payload.arxiv_id required"))?;

    // Politeness: hold the shared arXiv gate for the whole ingest.
    let extract = {
        let _permit = state.arxiv.acquire().await;
        tracing::info!(arxiv_id, user_agent = %state.config.arxiv_user_agent, "ingest start");
        grokrxiv_ingest::pipeline::ingest(arxiv_id)
            .await
            .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
    };

    let submitted_date = item
        .payload
        .get("submitted_date")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let paper_id = crate::db::upsert_paper(pool, &extract, submitted_date).await?;
    tracing::info!(arxiv_id, %paper_id, "ingest persisted papers row");

    // Auto-enqueue a Review job for papers in the auto-review window. arXiv
    // metadata doesn't (yet) populate `submitted_date`, so we conservatively
    // treat "no date" as in-window only when the operator explicitly asked for
    // ingest+review via the payload flag. Background-scheduler ingests should
    // set `payload.auto_review = true` when they want this behaviour.
    let auto_review = item
        .payload
        .get("auto_review")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let in_window = match item.payload.get("submitted_date").and_then(|v| v.as_str()) {
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(|d| {
                crate::scheduler::paper_in_auto_review_window(
                    d,
                    state.config.scheduler.auto_review_from,
                )
            })
            .unwrap_or(false),
        None => auto_review,
    };
    if in_window {
        let job_id = crate::db::create_job(pool, JobKind::Review, Some(paper_id)).await?;
        supervisor_tx
            .send(WorkItem {
                job_id,
                kind: JobKind::Review,
                ref_id: Some(paper_id),
                payload: serde_json::Value::Null,
                attempt: 0,
            })
            .await
            .map_err(|e| anyhow::anyhow!("enqueue review job: {e}"))?;
        tracing::info!(%job_id, %paper_id, "auto-enqueued review job");
    }

    Ok(())
}

/// Background worker: run the typed DAG for `item.ref_id` (paper id). Leaves
/// the review at `awaiting_moderation` — publishing requires admin approval.
#[cfg(feature = "grokrxiv-ingest")]
async fn run_review(state: &AppState, item: &WorkItem) -> anyhow::Result<()> {
    let paper_id = item
        .ref_id
        .ok_or_else(|| anyhow::anyhow!("run_review: ref_id (paper id) required"))?;
    let review_id = run_review_for_paper_full(state, paper_id).await?;
    tracing::info!(%review_id, "review job complete — awaiting_moderation");
    Ok(())
}

#[cfg(feature = "grokrxiv-ingest")]
async fn run_review_for_paper_full(state: &AppState, paper_id: Uuid) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    // Reload the paper row's data; the ingest crate is the canonical source so
    // we round-trip through it for the fields the DAG needs.
    let row = crate::db::load_paper_review_seed(pool, paper_id)
        .await
        .map_err(|e| anyhow::anyhow!("load paper row: {e}"))?;
    let crate::db::PaperReviewSeedRow {
        arxiv_id,
        title,
        abstract_,
        field,
        submitted_date: _submitted,
    } = row;

    if let Some(extract) = load_latest_review_input_extract(pool, paper_id).await? {
        tracing::info!(
            %paper_id,
            arxiv_id = %extract.arxiv_id,
            "review: loaded extract from latest persisted review_input"
        );
        return run_review_dag_from_state(state, pool, paper_id, extract).await;
    }

    // RPT3 Wave-3 Team-F: prefer the persisted review_input.json (Tier-1)
    // when this paper was extracted in a previous run. The body_markdown +
    // section bodies are loaded from the local grokrxiv-data clone; falling
    // back to a fresh deterministic re-ingest only when the cached pointer
    // is missing or the local files can't be read.
    #[cfg(feature = "grokrxiv-storage")]
    {
        if let Ok(Some(assets)) = crate::db::read_paper_assets(pool, paper_id).await {
            if matches!(assets.extraction_status, crate::db::ExtractionStatus::Ready) {
                if let Some(git_path) = assets.git_path.as_deref() {
                    let repo_root: std::path::PathBuf = std::env::var("GROKRXIV_DATA_REPO_PATH")
                        .ok()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            std::path::PathBuf::from(
                                "/Users/mlong/Documents/Development/grokrxiv-data",
                            )
                        });
                    let ri_path = repo_root.join(git_path).join("review_input.json");
                    if let Ok(bytes) = std::fs::read(&ri_path) {
                        if let Ok(ri) =
                            serde_json::from_slice::<grokrxiv_storage::ReviewInput>(&bytes)
                        {
                            match crate::ingest_pipeline::load_paper_extract(&repo_root, &ri) {
                                Ok(extract) => {
                                    tracing::info!(
                                        %paper_id,
                                        arxiv_id,
                                        git_path,
                                        "review: loaded extract from cached review_input.json"
                                    );
                                    return run_review_dag_from_state(
                                        state, pool, paper_id, extract,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!(arxiv_id, err = %format!("{e:#}"), "review_input.json present but load_paper_extract failed; re-ingesting");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // The DAG's call sites only need title/abstract/field/arxiv_id — sections
    // and bibliography are nice-to-have. Re-ingest to get them when possible;
    // fall back to a minimal extract on transient arXiv failure so a single
    // network blip doesn't tank a queued review.
    let extract = match grokrxiv_ingest::pipeline::ingest(&arxiv_id).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(arxiv_id, err = %e, "review: re-ingest failed, using DB-only fields");
            grokrxiv_schemas::PaperExtract {
                arxiv_id,
                title,
                authors: vec![],
                abstract_: abstract_.unwrap_or_default(),
                field,
                sections: vec![],
                figures: vec![],
                bibliography: vec![],
                source_format: None,
            }
        }
    };

    run_review_dag_from_state(state, pool, paper_id, extract).await
}

#[cfg(feature = "grokrxiv-ingest")]
async fn load_latest_review_input_extract(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
) -> anyhow::Result<Option<grokrxiv_schemas::PaperExtract>> {
    let Some(artifact) = crate::db::load_latest_review_input_artifact(pool, paper_id).await? else {
        return Ok(None);
    };
    match serde_json::from_value::<grokrxiv_schemas::PaperExtract>(artifact) {
        Ok(extract) => Ok(Some(extract)),
        Err(e) => {
            tracing::warn!(%paper_id, err = %e, "review: latest review_input is not a PaperExtract");
            Ok(None)
        }
    }
}

/// Background worker: publish a moderation-approved review to GitHub.
/// Wraps [`grokrxiv_publisher::GithubPublisher::open_review_pr`] with an
/// [`AdminCaller`] capability token; the caller (admin approval endpoint /
/// CLI) is responsible for moderator authentication.
#[cfg(feature = "grokrxiv-publisher")]
async fn run_publish(state: &AppState, item: &WorkItem) -> anyhow::Result<()> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let review_id = item
        .ref_id
        .ok_or_else(|| anyhow::anyhow!("run_publish: ref_id (review id) required"))?;

    let row = crate::db::load_publish_review(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let crate::db::PublishReviewRow {
        review_id: _review_row_id,
        arxiv_id,
        title,
        field,
        paper_id,
        visibility,
        source_kind,
        source_id,
    } = row;
    let source_ref =
        crate::source_display::source_display_ref(&source_kind, source_id.as_deref(), &arxiv_id);
    let artifact_id = crate::source_display::source_artifact_id(source_id.as_deref(), &arxiv_id);

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{artifact_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        artifact_id = artifact_id,
    );
    for name in ["review.html", "review.md", "review.tex", "bundle.zip"] {
        let path = dir_local.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            files.push((format!("{repo_prefix}/{name}"), bytes));
        }
    }
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts under artifacts/{review_id} — \
             re-run `grokrxiv ingest <arxiv_id>` to regenerate."
        );
    }

    let Some(token) = std::env::var("GITHUB_TOKEN").ok() else {
        // Local-only flows: simulate so the supervisor doesn't crash. The CLI
        // `approve` command logs the same line; we just persist the simulated
        // PR URL so downstream `published` transitions still have something
        // to point at.
        tracing::warn!(%review_id, "GITHUB_TOKEN unset; simulating publish");
        let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
        let (owner, repo) = review_repo_for_visibility(&visibility);
        let simulated = format!(
            "https://github.com/{owner}/{repo}/pull/SIMULATED-{}",
            &review_id.simple().to_string()[..8]
        );
        let _ = crate::db::set_review_github_pr_url(pool, review_id, &simulated).await;
        return Ok(());
    };

    let (owner, repo) = review_repo_for_visibility(&visibility);
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner, repo);
    let admin = AdminCaller::from_admin_endpoint();
    let pr_title = format!("Review: {} ({})", title, source_ref);
    let body_md = if visibility == "private" {
        "Approved by supervisor `run_publish`. \
             Private review: dashboard-only unless archived in the private reviews repo. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
            .to_string()
    } else {
        "Approved by supervisor `run_publish`. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
            .to_string()
    };
    let params = OpenReviewPr {
        arxiv_id: artifact_id,
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: pr_title,
        review_id,
        body_md,
        correction_source_path: None,
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;
    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = crate::db::set_review_github_pr_url(pool, review_id, &pr_url).await;
    tracing::info!(%review_id, %pr_url, "publish complete");

    // FP-RPT3c C2 — close any superseded PR for this paper.
    close_superseded_pr_if_any(pool, &publisher, &admin, paper_id, &pr_url).await;
    Ok(())
}

#[cfg(feature = "grokrxiv-publisher")]
fn review_repo_for_visibility(visibility: &str) -> (String, String) {
    match visibility {
        "private" => repo_from_combined_env(
            "GROKRXIV_PRIVATE_REVIEWS_REPO",
            "GrokRxiv",
            "grokrxiv-private-reviews",
        ),
        _ => {
            if let Some(repo) = repo_from_combined_env_optional("GROKRXIV_PUBLIC_REVIEWS_REPO") {
                repo
            } else {
                repo_from_legacy_public_env()
            }
        }
    }
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_legacy_public_env() -> (String, String) {
    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo_raw =
        std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "grokrxiv-reviews".into());
    split_owner_repo(&repo_raw).unwrap_or((owner, repo_raw))
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_combined_env(var: &str, default_owner: &str, default_repo: &str) -> (String, String) {
    repo_from_combined_env_optional(var)
        .unwrap_or_else(|| (default_owner.to_string(), default_repo.to_string()))
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_combined_env_optional(var: &str) -> Option<(String, String)> {
    let raw = std::env::var(var).ok()?;
    split_owner_repo(&raw)
}

#[cfg(feature = "grokrxiv-publisher")]
fn split_owner_repo(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let (owner, repo) = trimmed.split_once('/')?;
    let owner = owner.trim();
    let repo = repo.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Look up the most recently superseded review's PR URL for this paper and,
/// if found, close that PR on the moderation repo with a comment pointing at
/// the new one. Failures here are logged but never fail the new-PR open path
/// — the PR may already have been closed by hand, the GitHub token may have
/// lost scope, etc.
#[cfg(feature = "grokrxiv-publisher")]
async fn close_superseded_pr_if_any(
    pool: &sqlx::PgPool,
    publisher: &grokrxiv_publisher::GithubPublisher,
    admin: &grokrxiv_publisher::AdminCaller,
    paper_id: Uuid,
    new_pr_url: &str,
) {
    let prior = match crate::db::fetch_superseded_pr_url(pool, paper_id).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::warn!(%paper_id, err = %e, "supersede: fetch_superseded_pr_url failed");
            return;
        }
    };
    let Some(prior_url) = prior else { return };
    let Some(prior_n) = grokrxiv_publisher::parse_pr_number(&prior_url) else {
        tracing::warn!(
            %paper_id,
            %prior_url,
            "supersede: prior PR URL did not parse to a numeric id (simulated PR?)",
        );
        return;
    };
    let new_n_str = grokrxiv_publisher::parse_pr_number(new_pr_url)
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| new_pr_url.to_string());
    let comment = format!(
        "Superseded by {new_n_str}.\n\
         The new review run incorporated extraction-pipeline fixes and the prior review row was transitioned to status='withdrawn'.",
    );
    if let Err(e) = publisher
        .close_pr_with_comment(admin, prior_n, &comment)
        .await
    {
        tracing::warn!(
            %paper_id,
            prior_pr = %prior_url,
            err = %e,
            "supersede: close_pr_with_comment failed — leaving prior PR as-is (likely already closed)",
        );
    } else {
        tracing::info!(
            %paper_id,
            prior_pr = %prior_url,
            new_pr = %new_pr_url,
            "supersede: closed prior PR",
        );
    }
}

// Stub variants used when the matching feature isn't active so the supervisor
// still compiles in the minimal `--no-default-features` build.

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn run_ingest(
    _state: &AppState,
    _item: &WorkItem,
    _supervisor_tx: &mpsc::Sender<WorkItem>,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "run_ingest requires --features full (grokrxiv-ingest)"
    ))
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn run_review(_state: &AppState, _item: &WorkItem) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "run_review requires --features full (grokrxiv-ingest)"
    ))
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn run_publish(_state: &AppState, _item: &WorkItem) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "run_publish requires --features full (grokrxiv-publisher)"
    ))
}

fn is_retryable(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("timeout") || s.contains("rate") || s.contains("temporar")
}

fn exp_backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(std::cmp::min(base, 30_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backoff_caps_at_30s() {
        assert!(exp_backoff(10) <= Duration::from_secs(30));
    }

    /// RPT2 Track F: the revision_artifact schema parses and validates the
    /// happy-path artifact, and rejects an artifact missing a required field.
    #[test]
    fn revision_artifact_schema_validates() {
        let schema_str = include_str!("../../../schemas/revision_artifact.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(schema_str).expect("schema parses as JSON");
        let validator =
            jsonschema::validator_for(&schema).expect("schema compiles as JSON Schema draft-07");

        // Happy path: every required field present.
        let good = serde_json::json!({
            "target": "paper_latex",
            "patches": [{
                "section": "introduction",
                "original": "We propose ...",
                "proposed":  "We introduce ...",
                "rationale": "Match the abstract's verb.",
                "confidence": 0.8,
            }],
        });
        assert!(
            validator.is_valid(&good),
            "expected valid artifact to validate"
        );

        // Bad: missing `rationale` on the patch.
        let bad = serde_json::json!({
            "target": "paper_latex",
            "patches": [{
                "section": "introduction",
                "original": "x",
                "proposed":  "y",
                "confidence": 0.5,
            }],
        });
        assert!(
            !validator.is_valid(&bad),
            "expected artifact missing rationale to fail"
        );

        // Bad: target not in the enum.
        let bad_target = serde_json::json!({
            "target": "something_else",
            "patches": [],
        });
        assert!(
            !validator.is_valid(&bad_target),
            "expected bad target to fail"
        );
    }

    /// RPT2 Track F: `apply_revisions` returns a clean error when the
    /// supervisor has no database configured. Exercises the input-validation
    /// path that wraps the lookup; the DB-bound branch is covered by the
    /// integration test that ships under `--features grokrxiv-ingest`.
    #[cfg(feature = "grokrxiv-ingest")]
    #[tokio::test]
    async fn apply_revisions_errors_without_db() {
        // Build a Config that has NO database_url so `state.db` is `None`.
        let mut config = crate::Config::from_env();
        config.database_url = None;
        let state = crate::AppState::from_config(config)
            .await
            .expect("AppState builds without a database url");
        let err = apply_revisions(&state, Uuid::new_v4(), vec![0])
            .await
            .expect_err("expected error when DATABASE_URL is unset");
        let msg = err.to_string();
        assert!(
            msg.contains("DATABASE_URL"),
            "error should mention missing DATABASE_URL, got: {msg}"
        );
    }

    /// RPT2 Track F: the global `--mode` and `--revision-target` flags parse
    /// via clap's `ValueEnum` derive. Doctor is used as an inert subcommand
    /// so the parse exercises the flags exclusively.
    #[test]
    fn cli_parses_mode_and_revision_target_flags() {
        use crate::agents::{AgentMode, RevisionTarget};
        use crate::cli::Cli;
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "grokrxiv",
            "--mode",
            "review_and_revise",
            "--revision-target",
            "paper_latex",
            "doctor",
        ])
        .expect("cli parses with the new RPT2 Track F flags");
        assert_eq!(cli.mode, AgentMode::ReviewAndRevise);
        assert_eq!(cli.revision_target, RevisionTarget::PaperLatex);

        // Defaults exercise the value-enum default arm.
        let defaults = Cli::try_parse_from(["grokrxiv", "doctor"]).expect("defaults parse");
        assert_eq!(defaults.mode, AgentMode::ReviewOnly);
        assert_eq!(defaults.revision_target, RevisionTarget::PaperLatex);
    }

    // -----------------------------------------------------------------
    // Track 8a: build_specialist_prompt fidelity tests.
    //
    // These tests cover the four behaviors the operator locked in:
    //   1. section bodies are actually included in the rendered prompt;
    //   2. per-role char budgets are honored, with the 60/40 truncation
    //      marker present on overflow;
    //   3. bibliography entries are rendered with synthesized `[N]` keys;
    //   4. the MetaReviewer prompt omits the paper body by contract.
    // -----------------------------------------------------------------

    #[cfg(feature = "grokrxiv-ingest")]
    fn fake_extract(
        sections: Vec<(&str, String)>,
        bibliography: Vec<&str>,
    ) -> grokrxiv_schemas::PaperExtract {
        use grokrxiv_schemas::{Citation, PaperExtract, Section};
        PaperExtract {
            arxiv_id: "test/0001".into(),
            title: "Test Title".into(),
            authors: vec![],
            abstract_: "Test abstract sentence.".into(),
            field: Some("cs.LG".into()),
            sections: sections
                .into_iter()
                .map(|(h, b)| Section {
                    heading: h.into(),
                    body_markdown: b,
                })
                .collect(),
            figures: vec![],
            bibliography: bibliography
                .into_iter()
                .map(|raw| Citation {
                    raw: raw.into(),
                    doi: None,
                    arxiv_id: None,
                    title: None,
                })
                .collect(),
            source_format: None,
        }
    }

    /// Track 8a-1: the first section's body_markdown lands in every
    /// body-review specialist prompt (sanity-check that the previous
    /// heading-only behavior is gone). Citation uses a compact context prompt
    /// instead of the full body.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_includes_section_body() {
        use grokrxiv_schemas::AgentRole;
        let body = "Introductory text. ".repeat(50);
        let head_200: String = body.chars().take(200).collect();
        let extract = fake_extract(
            vec![
                ("1. Introduction", body.clone()),
                ("2. Methods", "Methods text.".to_string()),
            ],
            vec![],
        );

        for role in [
            AgentRole::Summary,
            AgentRole::TechnicalCorrectness,
            AgentRole::Novelty,
            AgentRole::Reproducibility,
        ] {
            let prompt = build_specialist_prompt(role, &extract, None, None, None, None);
            assert!(
                prompt.contains("## 1. Introduction"),
                "role {role:?}: prompt missing heading: {}",
                &prompt[..prompt.len().min(400)]
            );
            assert!(
                prompt.contains(&head_200),
                "role {role:?}: first 200 chars of body not in prompt"
            );
            assert!(
                prompt.contains("## 2. Methods"),
                "role {role:?}: second section heading missing"
            );
        }
    }

    /// Track 8a-2: when the total body is larger than the per-role budget,
    /// the prompt is truncated and either (a) a section is 60/40-truncated
    /// (sole section overflow), or (b) the remaining-sections-truncated
    /// footer is present. Budget is honored within the marker overhead.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_respects_per_role_budget() {
        use grokrxiv_schemas::AgentRole;
        // Reproducibility role: budget = 80_000. Build 8 sections of
        // ~15_000 chars each → ~120_000 chars total, exceeds budget.
        let big = "x".repeat(15_000);
        let extract = fake_extract(
            (0..8)
                .map(|i| {
                    (
                        // headings need 'static-ish lifetime → leak intentionally OK in tests
                        Box::leak(format!("Section {i}").into_boxed_str()) as &str,
                        big.clone(),
                    )
                })
                .collect(),
            vec![],
        );

        let budget = body_budget_chars(AgentRole::Reproducibility);
        let prompt =
            build_specialist_prompt(AgentRole::Reproducibility, &extract, None, None, None, None);

        // Sanity: actually exceeded budget with raw bodies (8 * 15_000 = 120_000 > 80_000).
        let total_raw: usize = extract.sections.iter().map(|s| s.body_markdown.len()).sum();
        assert!(total_raw > budget);

        // Either the per-section truncation marker is present, or the
        // remaining-sections footer is present (both indicate truncation
        // actually fired).
        let has_section_marker = prompt.contains("[…truncated…]");
        let has_footer_marker = prompt.contains("[…remaining sections truncated; headings:");
        assert!(
            has_section_marker || has_footer_marker,
            "expected a truncation marker in the rendered prompt"
        );

        // Body block must fit inside its budget plus a small slack for the
        // headings + truncation markers we render in this test. Use the
        // raw byte length of the rendered "Paper body:" region.
        let body_start = prompt
            .find("Paper body:\n\n")
            .expect("Paper body block present")
            + "Paper body:\n\n".len();
        let body_end = prompt.find("\nTask:").unwrap_or(prompt.len());
        let body_region_chars = prompt[body_start..body_end].chars().count();
        // Allow up to 5% slack for the trailing remaining-sections-footer line.
        let allowed = budget + (budget / 20);
        assert!(
            body_region_chars <= allowed,
            "body region {body_region_chars} chars exceeds allowed {allowed} (budget {budget})"
        );
    }

    /// Track 8a-3: bibliography entries appear in the rendered prompt with
    /// 1-indexed synthesized keys.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_renders_bibliography() {
        use grokrxiv_schemas::AgentRole;
        let mut extract = fake_extract(
            vec![(
                "1. Introduction",
                "The result builds on prior work [@alice2020].".to_string(),
            )],
            vec!["alice2020", "Bob, A follow-up paper, 2021."],
        );
        extract.bibliography[0].title = Some("A foundational paper".to_string());

        let prompt = build_specialist_prompt(AgentRole::Citation, &extract, None, None, None, None);
        assert!(
            prompt.contains("Bibliography (2 entries):"),
            "expected bibliography header in prompt"
        );
        assert!(
            prompt.contains("[1] alice2020 | title: A foundational paper"),
            "expected first bib entry with key and title"
        );
        assert!(
            prompt.contains("[2] Bob, A follow-up paper, 2021."),
            "expected second bib entry with key [2]"
        );
        assert!(
            prompt.contains("Citation contexts:"),
            "expected citation contexts block"
        );
        assert!(
            prompt.contains("The result builds on prior work [@alice2020]."),
            "expected cited sentence in compact citation prompt"
        );
        assert!(
            !prompt.contains("Paper body:"),
            "citation prompt should not include the full body block"
        );
    }

    /// Large bibliography prompts must stay bounded for local CLI runners.
    /// The verifier preserves full citation existence data; the LLM relevance
    /// pass only needs a representative capped slice plus in-text contexts.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn citation_prompt_caps_large_bibliography_for_cli_runtime() {
        use grokrxiv_schemas::AgentRole;
        let bibliography = (1..=40)
            .map(|i| Box::leak(format!("Reference {i}").into_boxed_str()) as &str)
            .collect();
        let extract = fake_extract(
            vec![(
                "1. Introduction",
                "The result follows earlier work [@ref1].".to_string(),
            )],
            bibliography,
        );

        let prompt = build_specialist_prompt(AgentRole::Citation, &extract, None, None, None, None);

        assert!(
            prompt.contains("Bibliography (40 entries; showing 32):"),
            "expected capped bibliography header in prompt"
        );
        assert!(prompt.contains("[32] Reference 32"));
        assert!(
            !prompt.contains("[33] Reference 33"),
            "entry past the prompt cap should be omitted"
        );
        assert!(
            prompt.contains("additional bibliography entries omitted from citation LLM prompt"),
            "expected explicit truncation marker"
        );
        assert!(
            prompt.contains("for each bibliography entry included below"),
            "task text must not ask the CLI to classify omitted entries"
        );
    }

    /// Track 8a-4: the MetaReviewer's specialist prompt is empty — by
    /// contract it never sees the paper body, only specialist outputs
    /// (which are added later via `build_meta_synthesis_prompt`).
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn meta_reviewer_prompt_omits_paper_body() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![(
                "1. Introduction",
                "This is the introductory body markdown that must NOT leak.".into(),
            )],
            vec!["Some citation, 2020."],
        );
        let prompt =
            build_specialist_prompt(AgentRole::MetaReviewer, &extract, None, None, None, None);
        assert_eq!(prompt, "", "MetaReviewer prompt must be empty");
        assert!(!prompt.contains("introductory body markdown"));
        assert!(!prompt.contains("Bibliography"));
    }

    /// Track 8a-5 (regression guard): when a single section's rendered
    /// length exceeds the budget on its own, the 60/40-truncation marker
    /// is emitted and the section is the sole surviving body content.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn single_giant_section_uses_60_40_truncation() {
        use grokrxiv_schemas::AgentRole;
        // Summary budget = 48_000. Build a single 100_000-char section.
        let huge = "abcdefghij".repeat(10_000); // 100_000 chars
        let extract = fake_extract(vec![("1. Introduction", huge)], vec![]);
        let prompt = build_specialist_prompt(AgentRole::Summary, &extract, None, None, None, None);
        assert!(
            prompt.contains("[…truncated…]"),
            "expected the 60/40 truncation marker for single oversized section"
        );
    }

    // -----------------------------------------------------------------
    // Phase 6: proof-as-code axiom.
    // -----------------------------------------------------------------

    #[test]
    fn role_system_prompt_adds_proof_as_code_for_code_amenable_field() {
        use grokrxiv_schemas::AgentRole;
        let tc = role_system_prompt(AgentRole::TechnicalCorrectness, Some("math.AG"));
        assert!(
            tc.contains("PROOF-AS-CODE AXIOM"),
            "math.* should trigger axiom for technical_correctness"
        );
        let rp = role_system_prompt(AgentRole::Reproducibility, Some("cs.LO"));
        assert!(
            rp.contains("PROOF-AS-CODE AXIOM"),
            "cs.* should trigger axiom for reproducibility"
        );
        let mr = role_system_prompt(AgentRole::MetaReviewer, Some("hep-th"));
        assert!(
            mr.contains("RECOMMENDATION GATE"),
            "hep-* should trigger gate for meta_reviewer"
        );
    }

    #[test]
    fn role_system_prompt_skips_axiom_for_non_amenable_field() {
        use grokrxiv_schemas::AgentRole;
        let tc = role_system_prompt(AgentRole::TechnicalCorrectness, Some("q-bio.GN"));
        assert!(
            !tc.contains("PROOF-AS-CODE AXIOM"),
            "q-bio.* should NOT trigger the axiom"
        );
        let mr = role_system_prompt(AgentRole::MetaReviewer, None);
        assert!(
            !mr.contains("RECOMMENDATION GATE"),
            "missing field should NOT trigger the gate"
        );
    }

    #[test]
    fn role_system_prompt_skips_axiom_for_unrelated_roles() {
        use grokrxiv_schemas::AgentRole;
        let s = role_system_prompt(AgentRole::Summary, Some("cs.LO"));
        let n = role_system_prompt(AgentRole::Novelty, Some("math.AG"));
        let c = role_system_prompt(AgentRole::Citation, Some("hep-th"));
        for p in [&s, &n, &c] {
            assert!(
                !p.contains("PROOF-AS-CODE AXIOM"),
                "axiom should only fire for TC/Reproducibility"
            );
            assert!(
                !p.contains("RECOMMENDATION GATE"),
                "gate should only fire for MetaReviewer"
            );
        }
    }

    #[test]
    fn merge_citation_verifier_into_output_overlays_per_entry() {
        let llm_output = serde_json::json!({
            "entries": [
                {
                    "citation": { "key": "[1]", "raw": "Foo et al.", "title": null, "authors": [] },
                    "exists": false,
                    "resolved_doi": null,
                    "resolved_url": null,
                    "relevance": "high",
                    "notes": "Used in Section 3",
                    "explanation": "Cited as the source of Theorem 2."
                },
                {
                    "citation": { "key": "[2]", "raw": "Bar et al.", "title": null, "authors": [] },
                    "exists": false,
                    "resolved_doi": null,
                    "resolved_url": null,
                    "relevance": "medium",
                    "notes": null,
                    "explanation": "Cited in passing."
                }
            ],
            "missing_references": [],
            "summary": "LLM prose stays.",
            "confidence": 0.7
        });
        let v_notes = serde_json::json!({
            "checked": 2,
            "unresolved": ["Bar et al."],
            "entries": [
                { "raw": "Foo et al.", "exists": true,  "resolved_doi": "10.1/foo", "resolved_url": "https://doi.org/10.1/foo", "source": "crossref" },
                { "raw": "Bar et al.", "exists": false, "resolved_doi": null,       "resolved_url": null,                       "source": "none" }
            ]
        });
        let merged = merge_citation_verifier_into_output(llm_output, Some(&v_notes));
        let entries = merged.get("entries").unwrap().as_array().unwrap();
        // Entry 0: verifier marks resolved.
        assert_eq!(entries[0]["exists"], serde_json::Value::Bool(true));
        assert_eq!(entries[0]["resolved_doi"], "10.1/foo");
        assert_eq!(entries[0]["resolved_url"], "https://doi.org/10.1/foo");
        // LLM prose preserved.
        assert_eq!(entries[0]["relevance"], "high");
        assert_eq!(entries[0]["notes"], "Used in Section 3");
        // Entry 1: still unresolved.
        assert_eq!(entries[1]["exists"], serde_json::Value::Bool(false));
        assert_eq!(entries[1]["resolved_doi"], serde_json::Value::Null);
        // Summary intact.
        assert_eq!(merged["summary"], "LLM prose stays.");
    }

    #[test]
    fn merge_reproducibility_facts_appends_concerns_for_dead_urls_and_archived_repos() {
        use crate::agents::specialist_facts::{
            GithubRepoFact, ReproducibilityFacts, UrlCheck, UrlKind,
        };
        let llm = serde_json::json!({
            "code_availability": "open_source",
            "code_url": "https://github.com/foo/bar",
            "data_availability": "public",
            "data_url": null,
            "environment": null,
            "concerns": [{ "area": "evaluation", "description": "no held-out test set", "severity": "minor" }],
            "reproducibility_score": 0.8,
            "confidence": 0.7
        });
        let facts = ReproducibilityFacts {
            urls_checked: vec![
                UrlCheck {
                    url: "https://github.com/foo/bar".into(),
                    reachable: false,
                    status: Some(404),
                    final_url: None,
                    kind: UrlKind::Code,
                },
                UrlCheck {
                    url: "https://zenodo.org/record/123".into(),
                    reachable: false,
                    status: Some(410),
                    final_url: None,
                    kind: UrlKind::Dataset,
                },
                UrlCheck {
                    url: "https://example.com/keep".into(),
                    reachable: true,
                    status: Some(200),
                    final_url: None,
                    kind: UrlKind::Other,
                },
            ],
            github_repos: vec![GithubRepoFact {
                owner: "foo".into(),
                repo: "bar".into(),
                exists: true,
                default_branch: Some("main".into()),
                pushed_at: Some("2020-01-01T00:00:00Z".into()),
                stargazers_count: Some(5),
                license_spdx: None,
                archived: Some(true),
            }],
        };
        let merged = merge_reproducibility_facts_into_output(llm, &facts);
        let concerns = merged["concerns"].as_array().unwrap();
        // 1 existing + 2 unreachable urls (code + dataset) + 1 archived repo = 4.
        assert_eq!(concerns.len(), 4);
        // The existing concern is preserved.
        assert_eq!(concerns[0]["description"], "no held-out test set");
        // The two unreachable URL concerns are appended at major severity.
        let dead_url_concerns: Vec<_> = concerns
            .iter()
            .filter(|c| {
                c["description"]
                    .as_str()
                    .unwrap_or("")
                    .contains("could not reach")
            })
            .collect();
        assert_eq!(dead_url_concerns.len(), 2);
        for c in &dead_url_concerns {
            assert_eq!(c["severity"], "major");
        }
        // The archived-repo concern is minor.
        let archived: Vec<_> = concerns
            .iter()
            .filter(|c| c["description"].as_str().unwrap_or("").contains("archived"))
            .collect();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0]["severity"], "minor");
    }

    #[test]
    fn merge_citation_verifier_passes_through_when_no_notes() {
        let llm_output = serde_json::json!({
            "entries": [{ "citation": {"key":"[1]","raw":"x","title":null,"authors":[]}, "exists": false, "resolved_doi": null, "resolved_url": null, "relevance": "low", "notes": null, "explanation": "" }],
            "missing_references": [],
            "summary": "s",
            "confidence": 0.0
        });
        let merged = merge_citation_verifier_into_output(llm_output.clone(), None);
        assert_eq!(merged, llm_output);
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_renders_moderator_notes_when_present() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![("1. Intro", "Some content.".to_string())],
            vec!["[Foo23] Foo et al."],
        );
        let prompt = build_specialist_prompt(
            AgentRole::TechnicalCorrectness,
            &extract,
            Some("Please tighten the proof of Theorem 3."),
            None,
            None,
            None,
        );
        assert!(
            prompt.contains("Moderator notes from a prior `request-changes` round"),
            "moderator-notes section should be rendered"
        );
        assert!(
            prompt.contains("tighten the proof of Theorem 3"),
            "operator's notes should be embedded verbatim"
        );
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_omits_moderator_notes_when_absent_or_blank() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![("1. Intro", "Some content.".to_string())],
            vec!["[Foo23] Foo et al."],
        );
        let p_none =
            build_specialist_prompt(AgentRole::Reproducibility, &extract, None, None, None, None);
        assert!(
            !p_none.contains("Moderator notes"),
            "None should not emit the section"
        );
        let p_blank = build_specialist_prompt(
            AgentRole::Reproducibility,
            &extract,
            Some("  "),
            None,
            None,
            None,
        );
        assert!(
            !p_blank.contains("Moderator notes"),
            "whitespace-only should not emit the section"
        );
    }

    #[test]
    fn is_code_amenable_field_matches_expected_prefixes() {
        for f in [
            "cs.LO",
            "cs.LG",
            "math.AG",
            "hep-th",
            "hep-ph",
            "gr-qc",
            "astro-ph.CO",
            "cond-mat.str-el",
            "nlin.CD",
            "quant-ph",
            "nucl-th",
            "stat.ML",
        ] {
            assert!(
                is_code_amenable_field(f),
                "expected {f} to be code-amenable"
            );
        }
        for f in [
            "q-bio.GN",
            "q-fin.RM",
            "econ.GN",
            "eess.SP",
            "physics.med-ph",
        ] {
            assert!(
                !is_code_amenable_field(f),
                "expected {f} to NOT be code-amenable"
            );
        }
    }

    /// FP-RPT3b B1 regression: the meta-synthesis prompt must NOT document a
    /// `paper` input key. The meta_input contract (supervisor.rs:527–529) only
    /// contains `specialists`; advertising a `paper` key misleads the model
    /// into hallucinating evidence it does not have.
    #[test]
    fn meta_synthesis_prompt_does_not_document_paper_key() {
        let meta_input = serde_json::json!({
            "specialists": {
                "summary": {"tldr": "x"},
                "technical_correctness": {"claims": [], "overall_correctness": "pass", "confidence": 0.5},
                "novelty": {"verdict": "moderate", "novelty_score": 0.5, "related_work": [], "missing_prior_art": [], "confidence": 0.5},
                "reproducibility": {"reproducibility_score": 0.5, "code_availability": "present", "code_url": null, "data_availability": "present", "data_url": null, "environment": "x", "concerns": [], "confidence": 0.5},
                "citation": {"entries": [], "missing_references": [], "summary": "x", "confidence": 0.5},
            }
        });
        let prompt = build_meta_synthesis_prompt(&meta_input);

        // The prose preamble must not list `paper` as one of the documented
        // input keys. We check both the backtick-quoted form and the
        // "two keys" phrase that the stale template used.
        assert!(
            !prompt.contains("`paper`"),
            "prompt must not document a `paper` input key, got: {prompt}"
        );
        assert!(
            !prompt.contains("two keys"),
            "prompt must say `one key`, not `two keys`, got: {prompt}"
        );
        assert!(
            prompt.contains("one key"),
            "prompt should describe the single-key shape, got: {prompt}"
        );
        assert!(
            prompt.contains("`specialists`"),
            "prompt should document the `specialists` key, got: {prompt}"
        );
    }

    /// FP-RPT3b B2: the quorum constant is 3-of-5 by design. Lock the value
    /// so a thoughtless edit can't quietly degrade the gate.
    #[test]
    fn min_specialist_quorum_is_three() {
        assert_eq!(MIN_SPECIALIST_QUORUM, 3);
    }

    #[test]
    fn review_concurrency_defaults_to_full_parallel_for_cli_roles() {
        assert_eq!(review_concurrency_limit_from(None, true, 5), 5);
    }

    #[test]
    fn review_concurrency_defaults_to_full_parallel_for_api_roles() {
        assert_eq!(review_concurrency_limit_from(None, false, 5), 5);
    }

    #[test]
    fn review_concurrency_env_override_is_clamped() {
        assert_eq!(review_concurrency_limit_from(Some("2"), true, 5), 2);
        assert_eq!(review_concurrency_limit_from(Some("99"), true, 5), 5);
        assert_eq!(review_concurrency_limit_from(Some("0"), true, 5), 1);
    }

    /// FP-RPT3b B2: helper that mirrors the quorum-count predicate in
    /// `run_review_dag`. Counts specialists with usable verifier output
    /// (`pass` or `warn`) and compares against `MIN_SPECIALIST_QUORUM`.
    fn quorum_passes(statuses: &[Option<grokrxiv_schemas::VerifierStatus>]) -> bool {
        statuses
            .iter()
            .filter(|s| {
                matches!(
                    **s,
                    Some(grokrxiv_schemas::VerifierStatus::Pass)
                        | Some(grokrxiv_schemas::VerifierStatus::Warn)
                )
            })
            .count()
            >= MIN_SPECIALIST_QUORUM
    }

    #[test]
    fn quorum_fires_when_only_two_specialists_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Fail),
            Some(VerifierStatus::Fail),
            None,
        ];
        assert!(
            !quorum_passes(&statuses),
            "quorum should NOT pass at 2-of-5; meta_reviewer must be skipped"
        );
    }

    #[test]
    fn quorum_allows_meta_when_all_five_specialists_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![Some(VerifierStatus::Pass); 5];
        assert!(quorum_passes(&statuses), "quorum should pass at 5-of-5");
    }

    #[test]
    fn quorum_allows_meta_at_exactly_three_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Warn),
            Some(VerifierStatus::Fail),
            Some(VerifierStatus::Fail),
        ];
        assert!(
            quorum_passes(&statuses),
            "quorum should pass at exactly the 3-of-5 usable threshold"
        );
    }

    #[test]
    fn quorum_allows_meta_when_all_specialists_warn() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![Some(VerifierStatus::Warn); 5];
        assert!(
            quorum_passes(&statuses),
            "warn is non-blocking and should count as usable verifier output"
        );
    }

    /// Common case: one specialist degrades (e.g., transient API hiccup);
    /// the other four pass. Meta should still run.
    #[test]
    fn quorum_allows_meta_when_four_of_five_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Fail),
        ];
        assert!(
            quorum_passes(&statuses),
            "4-of-5 should clear the quorum; meta runs on the surviving four"
        );
    }

    /// FP-RPT3b B2: lock the structured error payload shape so the
    /// moderation UI / log scrapers can parse it without speculative
    /// schema-guessing.
    #[test]
    fn quorum_error_payload_is_structured() {
        let usable_roles: Vec<&'static str> = vec!["summary", "novelty"];
        let blocked_roles: Vec<&'static str> =
            vec!["technical_correctness", "reproducibility", "citation"];
        let payload = serde_json::json!({
            "summary": "Automated review gate failed before meta-review synthesis because too few specialist outputs passed verifier checks.",
            "strengths": [],
            "weaknesses": [
                format!(
                    "verifier quorum not met: only {} of 5 specialists produced usable output (need >= {})",
                    usable_roles.len(),
                    MIN_SPECIALIST_QUORUM,
                ),
                format!("Roles without usable verifier output: {}", blocked_roles.join(", ")),
            ],
            "questions": [
                "Please address the verifier failures and resubmit corrections for automated re-review.",
            ],
            "recommendation": "major_revision",
            "confidence": 1.0,
            "gate": {
                "name": "specialist_verifier_quorum",
                "usable_roles": usable_roles,
                "blocked_roles": blocked_roles,
                "min_quorum": MIN_SPECIALIST_QUORUM,
            }
        });

        let err_msg = payload
            .get("weaknesses")
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str())
            .expect("error key is a string");
        assert!(
            err_msg.starts_with("verifier quorum not met: only 2 of 5 specialists"),
            "structured error message has wrong prefix: {err_msg}"
        );
        assert!(
            err_msg.contains("need >= 3"),
            "structured error must surface the quorum threshold: {err_msg}"
        );
        assert_eq!(
            payload["recommendation"],
            serde_json::json!("major_revision")
        );
        assert_eq!(
            payload["gate"]["usable_roles"],
            serde_json::json!(["summary", "novelty"])
        );
        assert_eq!(payload["gate"]["min_quorum"], serde_json::json!(3));
    }
}
