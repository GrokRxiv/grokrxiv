use std::time::Duration;

use super::jobs::{exp_backoff, is_retryable};
use super::merge_facts::{
    merge_citation_verifier_into_output, merge_novelty_facts_into_output,
    merge_reproducibility_facts_into_output,
};
use super::prompts::{
    build_meta_synthesis_prompt, build_specialist_prompt, debug_prompt_root, dump_debug_prompt,
    role_system_prompt,
};
use super::rendering::render_to_disk;
use super::verification::{
    meta_failure_output, role_status_label, specialist_failure_output,
    validate_role_output_after_merge, verifier_status_mark, verify_artifact,
};
use super::{MAX_RETRIES, MIN_SPECIALIST_QUORUM};
use crate::cli_status::StatusMark;
use crate::state::AppState;
use serde_json::json;
use uuid::Uuid;

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_one_paper_full(state: &AppState, arxiv_id: &str) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    tracing::info!(arxiv_id, "M1: ingest start");
    crate::cli_status::emit_stage(1, 6, "Fetch", StatusMark::Run, "arXiv source and metadata");

    // With storage enabled, staged ingest persists review_input.json and the
    // review path uses that artifact as its source of truth.
    let (paper_id, extract);
    #[cfg(feature = "grokrxiv-storage")]
    {
        let opts = ingest_options_from_env();
        crate::cli_status::emit_stage(
            2,
            6,
            "Extract",
            StatusMark::Run,
            "staged extraction pipeline",
        );
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
    crate::cli_status::emit_stage(2, 6, "Extract", StatusMark::Ok, "paper artifacts persisted");
    crate::cli_status::emit_stage(
        3,
        6,
        "Review DAG",
        StatusMark::Run,
        "starting specialist reviewers",
    );

    run_review_dag_from_state(state, pool, paper_id, extract).await
}

#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
pub(super) fn ingest_options_from_env() -> crate::ingest_pipeline::IngestOptions {
    crate::ingest_pipeline::IngestOptions::from_env()
}

/// Drive the review DAG for a paper row that is already present in the database.

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_from_state(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    run_review_dag_from_state_with_context(state, pool, paper_id, extract, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_from_state_with_context(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
    let provider = state
        .providers
        .as_ref()
        .map(|registry| registry.default.clone());
    run_review_dag_inner_with_context(state, pool, provider, paper_id, extract, submission).await
}

// CLI runner overrides are passed through environment variables before review dispatch.
// Format:
//   GROKRXIV_RUNNER_OVERRIDE        = "cli" | "api" | "cloud" | "local_inference"
//   GROKRXIV_RUNNER_OVERRIDE_<ROLE> = same enum, per role
#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn review_runner_override_for(
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
pub(super) fn review_cache_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    ) || matches!(
        std::env::var("GROKRXIV_INGEST_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    )
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_with_job_tracking(
    state: &AppState,
    paper_id: Uuid,
    job_id: Uuid,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let mut attempt = 0;
    loop {
        crate::db::mark_running(pool, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("mark review job running: {e}"))?;
        match run_review_for_paper_full(state, paper_id).await {
            Ok(review_id) => {
                crate::db::mark_done(pool, job_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("mark review job done: {e}"))?;
                return Ok(review_id);
            }
            Err(e) if attempt + 1 < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = exp_backoff(attempt);
                tracing::warn!(
                    %job_id,
                    %paper_id,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    err = %format!("{e:#}"),
                    "blocking review failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                let error = format!("{e:#}");
                crate::db::mark_failed(pool, job_id, &error)
                    .await
                    .map_err(|mark_err| {
                        anyhow::anyhow!(
                            "mark review job failed: {mark_err}; original error: {error}"
                        )
                    })?;
                return Err(e);
            }
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_extract_with_job_tracking(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    job_id: Uuid,
) -> anyhow::Result<Uuid> {
    let mut attempt = 0;
    loop {
        crate::db::mark_running(pool, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("mark review job running: {e}"))?;
        match run_review_dag_from_state(state, pool, paper_id, extract.clone()).await {
            Ok(review_id) => {
                crate::db::mark_done(pool, job_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("mark review job done: {e}"))?;
                return Ok(review_id);
            }
            Err(e) if attempt + 1 < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = exp_backoff(attempt);
                tracing::warn!(
                    %job_id,
                    %paper_id,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    err = %format!("{e:#}"),
                    "blocking extract review failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                let error = format!("{e:#}");
                crate::db::mark_failed(pool, job_id, &error)
                    .await
                    .map_err(|mark_err| {
                        anyhow::anyhow!(
                            "mark review job failed: {mark_err}; original error: {error}"
                        )
                    })?;
                return Err(e);
            }
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn specialist_review_concurrency_limit(roles: &[grokrxiv_schemas::AgentRole]) -> usize {
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
pub(super) fn review_concurrency_limit_from(
    raw: Option<&str>,
    _has_cli_role: bool,
    max: usize,
) -> usize {
    let max = max.max(1);
    if let Some(parsed) = raw.and_then(|s| s.trim().parse::<usize>().ok()) {
        return parsed.clamp(1, max);
    }
    max
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_inner(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: Option<std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    run_review_dag_inner_with_context(state, pool, provider, paper_id, extract, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
#[derive(Debug, Clone)]
pub(super) struct ReviewSubmissionContext {
    /// Account user that requested the review, if it came from the web app.
    pub submitted_by: Option<Uuid>,
    /// Review visibility to persist on the `reviews` row.
    pub visibility: String,
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_inner_with_context(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: Option<std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
    use crate::agents::runners::api::ApiRunner;
    use crate::agents::{
        build_agent, AgentInput, AgentRunner, AgentRunnerKind, AgentSchema, AgentSpec,
        ConfiguredAgent, SandboxPolicy,
    };
    use grokrxiv_schemas::{AgentRole, MetaReview, VerifierStatus};
    use serde_json::json;
    use std::sync::Arc;

    let default_model = state.config.preview_model.clone();

    // Resolve the configured agent + runner pair for a role. Prefers the
    // boot-time registry built from `agents/*.yaml`; falls back only for
    // builds where the verifier-backed registry is unavailable. API fallback
    // needs a provider, but CLI fallback can run through local subscriptions.
    let make_fallback = |role: AgentRole,
                         schema: AgentSchema|
     -> anyhow::Result<(
        Arc<ConfiguredAgent>,
        Arc<dyn AgentRunner>,
        String,
        AgentRunnerKind,
    )> {
        let runner_kind = review_runner_override_for(role).unwrap_or(AgentRunnerKind::Cli);
        let model = crate::runtime_config::model_override_for_role(role)
            .unwrap_or_else(|| default_model.clone());
        let spec = AgentSpec {
            role,
            runner: runner_kind,
            sandbox: SandboxPolicy::None,
            provider: "claude".to_string(),
            model: model.clone(),
            schema,
            max_retries: 2,
            timeout_secs: 180,
        };
        let agent = Arc::new(build_agent(spec));
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
        Ok((agent, runner, model, runner_kind))
    };

    let resolve_agent = |role: AgentRole| -> anyhow::Result<(
        Arc<ConfiguredAgent>,
        Arc<dyn AgentRunner>,
        String,
        AgentRunnerKind,
    )> {
        if let Some(agent) = state.agents.get(&role) {
            let model = agent.spec().model.clone();
            // Runtime override beats YAML's runner: field for this run.
            let runner_kind = review_runner_override_for(role).unwrap_or(agent.spec().runner);
            if let Some(runner) = state.runners.get(&runner_kind) {
                return Ok((agent.clone(), runner.clone(), model, runner_kind));
            }
        }
        let schema = state
            .agent_schemas
            .get(&role)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing schema for role {role:?}"))?;
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
    let review_id = match submission.as_ref() {
        Some(context) => {
            crate::db::insert_review_with_submission(
                pool,
                paper_id,
                models_used,
                None,
                context.submitted_by,
                &context.visibility,
            )
            .await?
        }
        None => crate::db::insert_review(pool, paper_id, models_used, None).await?,
    };
    // Every review entering moderation gets a pending queue row for admin actions.
    let _ = crate::db::insert_moderation_pending(pool, review_id).await;
    tracing::info!(%review_id, "M1: review row created");
    crate::cli_status::emit(format!("review_id={review_id}"));

    // Drive the DAG inside an inner async block so any error path can
    // transition the review row off the stale `awaiting_moderation` state.
    // We use `withdrawn` because the DB enum has no `failed` value.
    let dag_result: anyhow::Result<()> = async {
    // The canonical topology remains data-backed while this function executes
    // the specialist fan-out, quorum gate, meta-reviewer, and render tail.
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
    crate::cli_status::emit_stage(
        3,
        6,
        "Review DAG",
        StatusMark::Run,
        &format!("{review_concurrency} specialist reviewers"),
    );
    let sem = Arc::new(tokio::sync::Semaphore::new(review_concurrency));
    let extract_arc = Arc::new(extract);
    let specialist_input: serde_json::Value =
        serde_json::to_value(extract_arc.as_ref()).unwrap_or_else(|_| json!({}));

    // Persist the shared specialist input artifact exactly once per review.
    crate::db::insert_review_input(pool, review_id, paper_id, &specialist_input).await?;

    // Hash the exact specialist input bytes so cache lookups match what each
    // role reasoned over.
    let specialist_content_hash =
        sha256_hex(&serde_json::to_vec(&specialist_input).unwrap_or_default());

    // Surface moderator notes from the latest request-changes pass.
    let moderator_notes: Option<String> = crate::db::fetch_latest_changes_request_notes(pool, paper_id)
        .await
        .unwrap_or(None);

    // Gather deterministic facts before specialist prompts so agents can use
    // verifier-side provenance instead of relying only on model memory.
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

    // Debug prompt dumps are best-effort and never fail the review.
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
        let (agent, runner, role_model, role_runner) = resolve_agent(role)?;
        let sem = sem.clone();
        let pool_cloned = pool.clone();
        let cache_hash = specialist_content_hash.clone();
        let specialist_input_cloned = specialist_input.clone();
        handles.push((role, role_model, role_runner, tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore alive");
            crate::cli_status::emit_detail(role_status_label(role), StatusMark::Run, "starting");

            // Only passed verifier rows are reused from cache.
            if !skip_review_cache {
                if let Ok(Some(hit)) =
                    crate::db::lookup_cache(&pool_cloned, paper_id, role, &cache_hash).await
                {
                    if hit.verifier_status == "pass" {
                        tracing::info!(
                            event = "cache",
                            role = super::role_slug(role),
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
                            hit.runner,
                            true,
                        ));
                    }
                }
            } else {
                tracing::info!(
                    event = "cache",
                    role = super::role_slug(role),
                    disabled = true,
                    "cache bypassed"
                );
            }
            tracing::info!(
                event = "cache",
                role = super::role_slug(role),
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
            let run =
                run_agent_with_supervisor_timeout(agent.as_ref(), runner.as_ref(), input).await?;
            anyhow::Ok((
                role,
                run.output,
                run.tokens_in,
                run.tokens_out,
                run.latency_ms,
                run.model,
                run.runner,
                false,
            ))
        })));
    }

    let mut specialist_results: Vec<(
        AgentRole,
        serde_json::Value,
        Option<i32>,
        Option<i32>,
        i32,
        String, // model actually used
        AgentRunnerKind,
        bool,   // cache hit
    )> = Vec::with_capacity(specialist_roles.len());
    for (role, role_model, role_runner, h) in handles {
        match h.await {
            Ok(Ok(result)) => specialist_results.push(result),
            Ok(Err(e)) => {
                let error = format!("{e:#}");
                tracing::warn!(
                    %review_id,
                    role = super::role_slug(role),
                    err = %error,
                    "specialist reviewer failed; recording failed verifier output"
                );
                crate::cli_status::emit_detail(role_status_label(role), StatusMark::Fail, &error);
                specialist_results.push((
                    role,
                    specialist_failure_output(role, &error),
                    None,
                    None,
                    0i32,
                    role_model,
                    role_runner,
                    false,
                ));
            }
            Err(e) => {
                let error = format!("specialist join: {e}");
                tracing::warn!(
                    %review_id,
                    role = super::role_slug(role),
                    err = %error,
                    "specialist reviewer task failed; recording failed verifier output"
                );
                crate::cli_status::emit_detail(role_status_label(role), StatusMark::Fail, &error);
                specialist_results.push((
                    role,
                    specialist_failure_output(role, &error),
                    None,
                    None,
                    0i32,
                    role_model,
                    role_runner,
                    false,
                ));
            }
        }
    }
    crate::cli_status::emit_stage(4, 6, "Verify", StatusMark::Run, "verifier ladder");

    // Persist and verify each specialist output, then capture verifier status
    // for the quorum gate before meta-review synthesis.
    let mut specialist_verifier_status: Vec<(AgentRole, Option<VerifierStatus>)> =
        Vec::with_capacity(specialist_results.len());
    for (role, output, tokens_in, tokens_out, latency_ms, used_model, used_runner, cache_hit) in
        &specialist_results
    {
        let (v_status, v_notes) = verify_artifact(state, &extract_arc, *role, output).await;
        // Verifier notes own citation existence and provenance; the persisted
        // LLM output remains schema-valid citation-use prose.
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
        #[cfg(feature = "grokrxiv-verifier")]
        if !matches!(v_status, Some(VerifierStatus::Fail)) {
            validate_role_output_after_merge(*role, &output_to_persist, &state.agent_schemas)?;
        }
        crate::db::insert_review_agent(
            pool,
            crate::db::ReviewAgentInsert {
                review_id,
                role: *role,
                runner: *used_runner,
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

        // Cache only fresh successful outputs.
        if !*cache_hit && v_status == Some(VerifierStatus::Pass) {
            let _ = crate::db::insert_cache(
                pool,
                paper_id,
                *role,
                &specialist_content_hash,
                output,
                "pass",
                used_model,
                *used_runner,
                *tokens_in,
                *tokens_out,
            )
            .await;
        }
        specialist_verifier_status.push((*role, v_status));
        crate::cli_status::emit_detail(
            role_status_label(*role),
            verifier_status_mark(v_status),
            "",
        );
        tracing::info!(role = ?role, latency_ms, model = %used_model, cache_hit, "M1: specialist persisted");
    }

    // The review gate decides whether the specialist set is usable for
    // meta-review synthesis.
    let specialist_gate = crate::review_gate::SpecialistGate::evaluate(
        &specialist_verifier_status,
        min_specialist_quorum,
        specialist_total,
    );
    let revision_source_hint = revision_target_source_path_hint(pool, paper_id, &extract_arc).await;
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
            },
            "revision_targets": []
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
        crate::cli_status::emit_detail(
            "meta reviewer",
            StatusMark::Fail,
            "specialist quorum not met",
        );
    } else {

    // Meta-review synthesis receives only specialist outputs keyed by role slug.
    let mut specialists_map = serde_json::Map::new();
    for (role, output, _ti, _to, _lat, _model, _runner, _cache_hit) in &specialist_results {
        specialists_map.insert(super::role_slug(*role).to_string(), output.clone());
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

    let (meta_agent, meta_runner, meta_model_used, meta_runner_used) =
        resolve_agent(AgentRole::MetaReviewer)?;
    crate::cli_status::emit_detail("meta reviewer", StatusMark::Run, "synthesis");

    // Meta-review cache keys on the specialist-output bundle.
    let meta_content_hash =
        sha256_hex(&serde_json::to_vec(&meta_input).unwrap_or_default());
    let mut meta_from_cache = false;
    let (
        meta_value,
        meta_tokens_in,
        meta_tokens_out,
        meta_latency_ms,
        meta_model_recorded,
        meta_runner_recorded,
    ) =
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
                    hit.runner,
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
                match run_agent_with_supervisor_timeout(
                    meta_agent.as_ref(),
                    meta_runner.as_ref(),
                    meta_agent_input,
                )
                .await
                {
                    Ok(run) => (
                        run.output,
                        run.tokens_in,
                        run.tokens_out,
                        run.latency_ms,
                        run.model,
                        run.runner,
                    ),
                    Err(e) => {
                        let error = format!("{e:#}");
                        tracing::warn!(
                            %review_id,
                            err = %error,
                            "meta reviewer failed; recording major_revision gate output"
                        );
                        crate::cli_status::emit_detail("meta reviewer", StatusMark::Fail, &error);
                        (
                            meta_failure_output(&error),
                            None,
                            None,
                            0i32,
                            meta_model_used.clone(),
                            meta_runner_used,
                        )
                    }
                }
            }
        };

    let meta_value = crate::revision_targets::enrich_meta_review(
        meta_value,
        &meta_input,
        revision_source_hint.as_deref(),
    );

    let (meta_v_status, meta_v_notes) =
        verify_artifact(state, &extract_arc, AgentRole::MetaReviewer, &meta_value).await;
    crate::db::insert_review_agent(
        pool,
        crate::db::ReviewAgentInsert {
            review_id,
            role: AgentRole::MetaReviewer,
            runner: meta_runner_recorded,
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
    crate::cli_status::emit_detail(
        "meta reviewer",
        verifier_status_mark(meta_v_status),
        "",
    );

    // Cache only fresh successful meta-reviews.
    if !meta_from_cache && meta_v_status == Some(VerifierStatus::Pass) {
        let _ = crate::db::insert_cache(
            pool,
            paper_id,
            AgentRole::MetaReviewer,
            &meta_content_hash,
            &meta_value,
            "pass",
            &meta_model_recorded,
            meta_runner_recorded,
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

    // Render artifacts from persisted review and agent rows.
    let _ = paper_id; // not needed by the new render path
    crate::cli_status::emit_stage(5, 6, "Render", StatusMark::Run, "review artifacts");
    if let Err(e) = render_to_disk(state, review_id).await {
        tracing::warn!(%review_id, err = %e, "render_to_disk failed");
        crate::cli_status::emit_stage(
            5,
            6,
            "Render",
            StatusMark::Warn,
            &format!("review artifacts: {e:#}"),
        );
    } else {
        crate::cli_status::emit_stage(5, 6, "Render", StatusMark::Ok, "review artifacts written");
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
        crate::cli_status::emit_stage(
            6,
            6,
            "Moderation",
            StatusMark::Fail,
            "review withdrawn after DAG failure",
        );
        return Err(e);
    }

    crate::cli_status::emit_stage(6, 6, "Moderation", StatusMark::Ok, "awaiting moderation");
    crate::cli_status::emit(format!("next: grokrxiv show {review_id}"));
    Ok(review_id)
}

/// Hex-encoded SHA-256 of the input bytes for review-output cache keys.

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_full(
    state: &AppState,
    paper_id: Uuid,
) -> anyhow::Result<Uuid> {
    run_review_for_paper_full_with_context(state, paper_id, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_full_with_context(
    state: &AppState,
    paper_id: Uuid,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
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
        return run_review_dag_from_state_with_context(state, pool, paper_id, extract, submission)
            .await;
    }

    // Prefer persisted review_input.json when staged extraction already produced it.
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
                                    return run_review_dag_from_state_with_context(
                                        state, pool, paper_id, extract, submission,
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

    run_review_dag_from_state_with_context(state, pool, paper_id, extract, submission).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn load_latest_review_input_extract(
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

#[cfg(feature = "grokrxiv-ingest")]
async fn revision_target_source_path_hint(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: &grokrxiv_schemas::PaperExtract,
) -> Option<String> {
    let row: Option<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
        "select coalesce(source_kind, 'arxiv'), source_id, source_metadata \
         from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some((source_kind, source_id, metadata)) = row else {
        return fallback_source_path_hint(extract);
    };
    if let Some(path) = metadata
        .get("correction_source_path")
        .and_then(|v| v.as_str())
    {
        if !path.trim().is_empty() {
            return Some(path.to_string());
        }
    }
    let adapter = metadata.get("adapter").unwrap_or(&serde_json::Value::Null);
    let raw_source_path = match source_kind.as_str() {
        "git_repo" => adapter.get("paper_path").and_then(|v| v.as_str()),
        "local_file" => adapter.get("path").and_then(|v| v.as_str()),
        "arxiv" => return correction_repo_path_hint(source_id.as_deref(), extract),
        _ => None,
    };
    raw_source_path
        .and_then(|path| correction_repo_path_from_raw(source_id.as_deref(), path))
        .or_else(|| raw_source_path.map(str::to_string))
        .or_else(|| fallback_source_path_hint(extract))
}

#[cfg(feature = "grokrxiv-ingest")]
fn correction_repo_path_hint(
    source_id: Option<&str>,
    extract: &grokrxiv_schemas::PaperExtract,
) -> Option<String> {
    let default_name = match extract.source_format.as_deref() {
        Some("tex") => "paper.tex",
        Some("pdf") => "paper.pdf",
        _ => return fallback_source_path_hint(extract),
    };
    correction_repo_path_from_raw(source_id, default_name)
        .or_else(|| Some(default_name.to_string()))
}

#[cfg(feature = "grokrxiv-ingest")]
fn correction_repo_path_from_raw(source_id: Option<&str>, raw_path: &str) -> Option<String> {
    let source_id = source_id?.trim();
    if source_id.is_empty() {
        return None;
    }
    let safe_source_id: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let file_name = std::path::Path::new(raw_path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())?;
    Some(format!("corrections/{safe_source_id}/{file_name}"))
}

#[cfg(feature = "grokrxiv-ingest")]
fn fallback_source_path_hint(extract: &grokrxiv_schemas::PaperExtract) -> Option<String> {
    match extract.source_format.as_deref() {
        Some("tex") => Some("paper.tex".to_string()),
        Some("pdf") => Some("paper.pdf".to_string()),
        _ => None,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_agent_with_supervisor_timeout(
    agent: &crate::agents::ConfiguredAgent,
    runner: &dyn crate::agents::AgentRunner,
    input: crate::agents::AgentInput,
) -> anyhow::Result<crate::agents::AgentRun> {
    let role = input.role;
    let spec = agent.spec();
    let timeout_secs = u64::from(spec.timeout_secs.max(1))
        .saturating_mul(u64::from(spec.max_retries).saturating_add(1))
        .max(1);
    let timeout_duration = Duration::from_secs(timeout_secs);
    tokio::time::timeout(timeout_duration, agent.run(runner, input))
        .await
        .map_err(|_| {
            anyhow::anyhow!("agent {role:?} timed out after {timeout_secs}s at supervisor level")
        })?
}
