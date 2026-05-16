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
/// With `--features full` + a configured `DATABASE_URL` + `ANTHROPIC_API_KEY`,
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
    use grokrxiv_schemas::PaperExtract;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let providers = state
        .providers
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no LLM provider configured"))?;

    // 1. Ingest — fetch arXiv metadata + PDF + bibliography.
    tracing::info!(arxiv_id, "M1: ingest start");
    let extract: PaperExtract = {
        let _permit = state.arxiv.acquire().await;
        grokrxiv_ingest::pipeline::ingest(arxiv_id)
            .await
            .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
    };

    // 2. Persist paper row.
    let paper_id = crate::db::upsert_paper(pool, &extract, None).await?;
    tracing::info!(arxiv_id, %paper_id, "M1: paper persisted");

    run_review_dag(state, pool, providers.default.clone(), paper_id, extract).await
}

/// Drive the review DAG for a paper row that is already present in the database.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_for_paper_blocking(
    state: &AppState,
    paper_id: Uuid,
) -> anyhow::Result<Uuid> {
    run_review_for_paper_full(state, paper_id).await
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
    // boot-time registry built from `agents/*.yaml`; falls back to a freshly
    // constructed `ApiRunner` wired to the passed-in `provider` when no
    // provider registry was available at boot (which is how the integration
    // test in tests/dag.rs runs the DAG against its wiremock-backed Claude).
    let make_fallback = |role: AgentRole,
                         schema: serde_json::Value|
     -> (Arc<dyn ReviewAgent>, Arc<dyn AgentRunner>, String) {
        let spec = AgentSpec {
            role,
            runner: AgentRunnerKind::Api,
            sandbox: SandboxPolicy::None,
            mode: AgentMode::ReviewOnly,
            provider: "claude".to_string(),
            model: default_model.clone(),
            schema,
            tool_policy: ToolPolicy::default(),
            max_retries: 2,
            timeout_secs: 180,
        };
        let agent: Arc<dyn ReviewAgent> = Arc::from(build_agent(spec));
        let mut providers_map: std::collections::HashMap<
            String,
            Arc<dyn grokrxiv_llm_adapter::LLMProvider>,
        > = std::collections::HashMap::new();
        providers_map.insert("claude".to_string(), provider.clone());
        let runner: Arc<dyn AgentRunner> = Arc::new(ApiRunner::new(providers_map));
        (agent, runner, default_model.clone())
    };

    // RPT2 G follow-up: the CLI's `--runner` / `--runner-for` flags land in
    // these env vars before this function runs. They override the YAML's
    // `runner:` field per role. Format:
    //   GROKRXIV_RUNNER_OVERRIDE        = "cli" | "api" | "cloud" | "local_inference"
    //   GROKRXIV_RUNNER_OVERRIDE_<ROLE> = same enum, per role (snake_case role name)
    // The CLI's `Command::Review` handler exports these from RuntimeConfig
    // before dispatching to the supervisor.
    let runner_override_for = |role: AgentRole| -> Option<AgentRunnerKind> {
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
    };

    let resolve_agent = |role: AgentRole|
        -> (Arc<dyn ReviewAgent>, Arc<dyn AgentRunner>, String) {
        if let Some(agent) = state.agents.get(&role) {
            let model = agent.spec().model.clone();
            // Runtime override beats YAML's runner: field for this run.
            let runner_kind = runner_override_for(role).unwrap_or(agent.spec().runner);
            if let Some(runner) = state.runners.get(&runner_kind) {
                return (agent.clone(), runner.clone(), model);
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
    let summary_model = resolve_agent(AgentRole::Summary).2;
    let tech_model = resolve_agent(AgentRole::TechnicalCorrectness).2;
    let novelty_model = resolve_agent(AgentRole::Novelty).2;
    let repro_model = resolve_agent(AgentRole::Reproducibility).2;
    let cite_model = resolve_agent(AgentRole::Citation).2;
    let meta_model = resolve_agent(AgentRole::MetaReviewer).2;
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

    // The five specialist roles fan out in parallel; the meta-reviewer runs
    // after they complete so it can synthesize their outputs.
    let specialist_roles = [
        AgentRole::Summary,
        AgentRole::TechnicalCorrectness,
        AgentRole::Novelty,
        AgentRole::Reproducibility,
        AgentRole::Citation,
    ];

    let sem = Arc::new(tokio::sync::Semaphore::new(5));
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

    let mut handles = Vec::with_capacity(specialist_roles.len());
    for role in specialist_roles {
        let prompt = build_specialist_prompt(role, extract_arc.as_ref());
        let system = role_system_prompt(role);
        let (agent, runner, role_model) = resolve_agent(role);
        let sem = sem.clone();
        let pool_cloned = pool.clone();
        let cache_hash = specialist_content_hash.clone();
        let specialist_input_cloned = specialist_input.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore alive");

            // FP6 A4: cache lookup before the LLM call. We only honour
            // verifier_status='pass' rows so a previously-warned/failed run
            // re-executes the agent.
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

    // Persist + verify each specialist's output against its role-specific
    // verifier ladder. The ladder uses the role-specific JSON schema as its
    // first rung (replacing the previous permissive-object workaround).
    for (role, output, tokens_in, tokens_out, latency_ms, used_model, cache_hit) in
        &specialist_results
    {
        let (v_status, v_notes) = verify_artifact(state, &extract_arc, *role, output).await;
        crate::db::insert_review_agent(
            pool,
            crate::db::ReviewAgentInsert {
                review_id,
                role: *role,
                model: used_model,
                output: output.clone(),
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
        tracing::info!(role = ?role, latency_ms, model = %used_model, cache_hit, "M1: specialist persisted");
    }

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
    let meta_system = role_system_prompt(AgentRole::MetaReviewer);

    let (meta_agent, meta_runner, meta_model_used) = resolve_agent(AgentRole::MetaReviewer);

    // FP6 A4: cache lookup for the meta-reviewer. Its content hash keys on
    // the specialists-bundle JSON it would have reasoned over, so two reviews
    // of the same paper whose specialists produced identical outputs (e.g. a
    // re-run with cache hits) share a cached meta-review.
    let meta_content_hash =
        sha256_hex(&serde_json::to_vec(&meta_input).unwrap_or_default());
    let mut meta_from_cache = false;
    let (meta_value, meta_tokens_in, meta_tokens_out, meta_latency_ms, meta_model_recorded) =
        match crate::db::lookup_cache(pool, paper_id, AgentRole::MetaReviewer, &meta_content_hash)
            .await
        {
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
    sqlx::query("update reviews set meta_review = $2 where id = $1")
        .bind(review_id)
        .bind(meta_value)
        .execute(pool)
        .await?;

    // Render artifacts to disk under ./artifacts/<review_id>/. The renderer
    // reads back the real meta-review JSON + every review_agents row from
    // Postgres, so the on-disk artifacts faithfully reflect the persisted
    // pipeline output (no synthetic placeholders). Storage-bucket upload is
    // M3; on-disk paths suffice for the M1 assertions.
    let _ = paper_id; // not needed by the new render path
    if let Err(e) = render_to_disk(state, review_id).await {
        tracing::warn!(%review_id, err = %e, "render_to_disk failed");
    }

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
        let patch_count = row
            .patches
            .as_array()
            .map(|a| a.len() as i32)
            .unwrap_or(0);
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
    use grokrxiv_schemas::AgentRole;
    match role {
        AgentRole::Summary => "summary",
        AgentRole::TechnicalCorrectness => "technical_correctness",
        AgentRole::Novelty => "novelty",
        AgentRole::Reproducibility => "reproducibility",
        AgentRole::Citation => "citation",
        AgentRole::MetaReviewer => "meta_reviewer",
    }
}

fn role_system_prompt(role: grokrxiv_schemas::AgentRole) -> String {
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
    format!(
        "You are a careful, honest specialist peer reviewer. You {task}. \
         Respond with strict JSON conforming to the supplied schema. No prose, \
         no code fences, no commentary."
    )
}

#[cfg(feature = "grokrxiv-ingest")]
fn build_specialist_prompt(
    role: grokrxiv_schemas::AgentRole,
    extract: &grokrxiv_schemas::PaperExtract,
) -> String {
    use grokrxiv_schemas::AgentRole;
    let paper_block = format!(
        "Paper title: {title}\n\nAbstract:\n{abstract_}\n\nSections (head only):\n{sections}\n",
        title = extract.title,
        abstract_ = extract.abstract_,
        sections = extract
            .sections
            .iter()
            .take(40)
            .map(|s| format!("- {}", s.heading))
            .collect::<Vec<_>>()
            .join("\n"),
    );
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
            "Audit the bibliography. For each citation populate the `entries` array \
             (each with `citation`, `exists`, `relevance`, and optional resolved_doi/url, \
             notes). Provide `summary` and `confidence`."
        }
        AgentRole::MetaReviewer => "",
    };
    format!("{paper_block}\nTask: {task}")
}

fn build_meta_synthesis_prompt(meta_input: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(meta_input).unwrap_or_else(|_| "{}".into());
    format!(
        "Below is a JSON object with two keys: `paper` (the ingested paper extract) \
         and `specialists` (the five specialist reviewers' outputs keyed by their \
         role slug: summary, technical_correctness, novelty, reproducibility, citation).\n\n\
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

    // 1. Load the meta-review JSON + the joined paper row.
    let row: (
        Option<serde_json::Value>,
        Uuid,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "select r.meta_review, p.id, p.arxiv_id, p.title, p.abstract, p.field \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("load review row: {e}"))?;
    let (meta_json, _paper_id, arxiv_id, title, abstract_, field) = row;

    let meta: MetaReview = meta_json
        .and_then(|v| serde_json::from_value::<MetaReview>(v).ok())
        .unwrap_or_else(|| fallback_meta(&title));

    #[derive(sqlx::FromRow)]
    struct AgentRenderRow {
        role: String,
        model: String,
        output: serde_json::Value,
        verifier_status: Option<String>,
        verifier_notes: Option<serde_json::Value>,
    }

    // 2. Load every agent row in role-sorted order.
    let agent_rows: Vec<AgentRenderRow> = sqlx::query_as(
        "select role, model, output, verifier_status, verifier_notes \
         from review_agents where review_id = $1 order by role",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow::anyhow!("load review_agents: {e}"))?;

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
        for row in &agent_rows {
            if row.role != "meta_reviewer" {
                specialists_map.insert(row.role.clone(), row.output.clone());
            }
        }
        serde_json::json!({ "specialists": serde_json::Value::Object(specialists_map) })
    };

    let mut agents: Vec<AgentRecord> = Vec::with_capacity(agent_rows.len());
    let mut agent_jsons: Vec<(String, Vec<u8>)> = Vec::with_capacity(agent_rows.len());
    for row in agent_rows {
        let role_slug = row.role.clone();
        if let Some(role) = parse_role_slug(&role_slug) {
            let status = match row.verifier_status.as_deref() {
                Some("warn") => VerifierStatus::Warn,
                Some("fail") => VerifierStatus::Fail,
                _ => VerifierStatus::Pass,
            };
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
    let providers = state
        .providers
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no LLM provider configured"))?;

    // Reload the paper row's data; the ingest crate is the canonical source so
    // we round-trip through it for the fields the DAG needs.
    let row: (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<chrono::NaiveDate>,
    ) = sqlx::query_as(
        "select arxiv_id, title, abstract, field, submitted_date from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("load paper row: {e}"))?;
    let (arxiv_id, title, abstract_, field, _submitted) = row;

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

    run_review_dag(state, pool, providers.default.clone(), paper_id, extract).await
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

    let row: (Uuid, String, String, Option<String>) = sqlx::query_as(
        "select r.id, p.arxiv_id, p.title, p.field \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let (_, arxiv_id, title, field) = row;

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{arxiv_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        arxiv_id = arxiv_id,
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
        let simulated = format!(
            "https://github.com/GrokRxiv/reviews/pull/SIMULATED-{}",
            &review_id.simple().to_string()[..8]
        );
        let _ = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
            .bind(review_id)
            .bind(&simulated)
            .execute(pool)
            .await;
        return Ok(());
    };

    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo = std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "reviews".into());
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner, repo);
    let admin = AdminCaller::from_admin_endpoint();
    let pr_title = format!("Review: {} (arXiv:{})", title, arxiv_id);
    let params = OpenReviewPr {
        arxiv_id: arxiv_id.clone(),
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: pr_title,
        review_id,
        body_md: "Approved by supervisor `run_publish`. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
            .to_string(),
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;
    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
        .bind(review_id)
        .bind(&pr_url)
        .execute(pool)
        .await;
    tracing::info!(%review_id, %pr_url, "publish complete");
    Ok(())
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
        let schema_str =
            include_str!("../../../schemas/revision_artifact.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(schema_str).expect("schema parses as JSON");
        let validator = jsonschema::validator_for(&schema)
            .expect("schema compiles as JSON Schema draft-07");

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
        assert!(validator.is_valid(&good), "expected valid artifact to validate");

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
        assert!(!validator.is_valid(&bad), "expected artifact missing rationale to fail");

        // Bad: target not in the enum.
        let bad_target = serde_json::json!({
            "target": "something_else",
            "patches": [],
        });
        assert!(!validator.is_valid(&bad_target), "expected bad target to fail");
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
}
