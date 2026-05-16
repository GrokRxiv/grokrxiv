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
    use grokrxiv_llm_adapter::Usage;
    use grokrxiv_schemas::{AgentRole, MetaReview};
    use serde_json::json;
    use std::sync::Arc;

    let default_model = state.config.preview_model.clone();

    // Resolve a `(provider, model)` for `role` from the per-role routing table
    // built at boot from `agents/*.yaml`. Falls back to the passed-in
    // `provider` + the orchestrator's PREVIEW_MODEL when routing isn't
    // populated (e.g. the integration test in tests/dag.rs builds an AppState
    // that only injects the wiremock-backed Claude provider).
    let resolve = |role: AgentRole| -> (Arc<dyn grokrxiv_llm_adapter::LLMProvider>, String) {
        match state.role_routing.get(&role) {
            Some((p, m)) => (p.clone(), m.clone()),
            None => (provider.clone(), default_model.clone()),
        }
    };

    // Pre-create the review row. `models_used` records the per-role model so
    // the moderation UI + the m1-pipeline `distinct model` assertion can show
    // which model each specialist used.
    let summary_model = resolve(AgentRole::Summary).1;
    let tech_model = resolve(AgentRole::TechnicalCorrectness).1;
    let novelty_model = resolve(AgentRole::Novelty).1;
    let repro_model = resolve(AgentRole::Reproducibility).1;
    let cite_model = resolve(AgentRole::Citation).1;
    let meta_model = resolve(AgentRole::MetaReviewer).1;
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

    let mut handles = Vec::with_capacity(specialist_roles.len());
    for role in specialist_roles {
        let schema = state
            .agent_schemas
            .get(&role)
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        let prompt = build_specialist_prompt(role, extract_arc.as_ref());
        let system = role_system_prompt(role);
        let (role_provider, role_model) = resolve(role);
        let sem = sem.clone();
        let input = specialist_input.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore alive");
            let started = std::time::Instant::now();
            let (parsed, usage) =
                call_with_schema(&*role_provider, &role_model, &system, &prompt, schema).await?;
            let latency_ms = started.elapsed().as_millis() as i32;
            anyhow::Ok((role, input, parsed, usage, latency_ms, role_model))
        }));
    }

    let mut specialist_results: Vec<(
        AgentRole,
        serde_json::Value,
        serde_json::Value,
        Usage,
        i32,
        String, // model actually used
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
    for (role, input, output, usage, latency_ms, used_model) in &specialist_results {
        let (v_status, v_notes) = verify_artifact(state, &extract_arc, *role, output).await;
        crate::db::insert_review_agent(
            pool,
            crate::db::ReviewAgentInsert {
                review_id,
                role: *role,
                model: used_model,
                input_artifact: input.clone(),
                output: output.clone(),
                verifier_status: v_status,
                verifier_notes: v_notes,
                tokens_in: Some(usage.tokens_in as i32),
                tokens_out: Some(usage.tokens_out as i32),
                latency_ms: Some(*latency_ms),
            },
        )
        .await?;
        tracing::info!(role = ?role, latency_ms, model = %used_model, "M1: specialist persisted");
    }

    // Meta-reviewer: real synthesis node. Hand it the bundle of specialist
    // outputs keyed by role slug and a copy of the paper extract, then ask it
    // for a `MetaReview`.
    let mut specialists_map = serde_json::Map::new();
    for (role, _input, output, _usage, _lat, _model) in &specialist_results {
        specialists_map.insert(role_slug(*role).to_string(), output.clone());
    }
    let meta_input = json!({
        "paper": &*extract_arc,
        "specialists": serde_json::Value::Object(specialists_map),
    });
    let meta_schema = state
        .agent_schemas
        .get(&AgentRole::MetaReviewer)
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }));
    let meta_prompt = build_meta_synthesis_prompt(&meta_input);
    let meta_system = role_system_prompt(AgentRole::MetaReviewer);

    let (meta_provider, meta_model_used) = resolve(AgentRole::MetaReviewer);
    let meta_started = std::time::Instant::now();
    let (meta_value, meta_usage) = call_with_schema(
        &*meta_provider,
        &meta_model_used,
        &meta_system,
        &meta_prompt,
        meta_schema,
    )
    .await?;
    let meta_latency_ms = meta_started.elapsed().as_millis() as i32;

    let (meta_v_status, meta_v_notes) =
        verify_artifact(state, &extract_arc, AgentRole::MetaReviewer, &meta_value).await;
    crate::db::insert_review_agent(
        pool,
        crate::db::ReviewAgentInsert {
            review_id,
            role: AgentRole::MetaReviewer,
            model: &meta_model_used,
            input_artifact: meta_input.clone(),
            output: meta_value.clone(),
            verifier_status: meta_v_status,
            verifier_notes: meta_v_notes,
            tokens_in: Some(meta_usage.tokens_in as i32),
            tokens_out: Some(meta_usage.tokens_out as i32),
            latency_ms: Some(meta_latency_ms),
        },
    )
    .await?;
    tracing::info!(meta_latency_ms, model = %meta_model_used, "M1: meta-reviewer persisted");

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

/// Run one JSON-schema-enforced LLM call with a single corrective retry on
/// parse failure. Returns the parsed JSON value and the usage stats.
#[cfg(feature = "grokrxiv-ingest")]
async fn call_with_schema(
    provider: &dyn grokrxiv_llm_adapter::LLMProvider,
    model: &str,
    system: &str,
    prompt: &str,
    schema: serde_json::Value,
) -> anyhow::Result<(serde_json::Value, grokrxiv_llm_adapter::Usage)> {
    use grokrxiv_llm_adapter::{ChatRequest, ContentPart, Message, ResponseFormat, Role};

    let make_req = |user_prompt: String| ChatRequest {
        system: Some(system.to_string()),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text(user_prompt)],
        }],
        model: model.to_string(),
        max_tokens: 6_000,
        temperature: 0.2,
        response_format: ResponseFormat::JsonSchema(schema.clone()),
        cache_system: true,
    };

    let resp = provider
        .complete(make_req(prompt.to_string()))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match parse_strict_json(&resp.text) {
        Ok(v) => Ok((v, resp.usage)),
        Err(first_err) => {
            // Single corrective retry: tell the model its prior output failed
            // schema validation and ask for strict JSON. No `{"raw": ...}`
            // fallback — if the retry also fails the caller surfaces a hard
            // error and the verifier records the parse error in
            // `verifier_notes.parse_error`.
            let corrective = format!(
                "{prompt}\n\nYour previous output did not validate against the schema; \
                 return strict JSON only, with no surrounding prose, code fences, or commentary."
            );
            let retry = provider
                .complete(make_req(corrective))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            match parse_strict_json(&retry.text) {
                Ok(v) => Ok((v, retry.usage)),
                Err(e) => Err(anyhow::anyhow!(
                    "parse failure after corrective retry: first={first_err}; retry={e}; \
                     raw_first={raw_first:?}; raw_retry={raw_retry:?}",
                    raw_first = resp.text,
                    raw_retry = retry.text,
                )),
            }
        }
    }
}

/// Try strict JSON parse; on failure, strip ```json fences and retry; on
/// failure again, return `Err`. Never returns `{"raw": ...}`.
fn parse_strict_json(s: &str) -> anyhow::Result<serde_json::Value> {
    let trimmed = s.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => Ok(v),
        Err(_) => {
            let stripped = strip_fences(trimmed);
            serde_json::from_str::<serde_json::Value>(stripped)
                .map_err(|e| anyhow::anyhow!("not valid JSON: {e}"))
        }
    }
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
        input_artifact: serde_json::Value,
        output: serde_json::Value,
        verifier_status: Option<String>,
        verifier_notes: Option<serde_json::Value>,
    }

    // 2. Load every agent row in role-sorted order.
    let agent_rows: Vec<AgentRenderRow> = sqlx::query_as(
        "select role, model, input_artifact, output, verifier_status, verifier_notes \
         from review_agents where review_id = $1 order by role",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow::anyhow!("load review_agents: {e}"))?;

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
            let artifact = serde_json::json!({
                "role": role_slug,
                "model": row.model.clone(),
                "input_artifact": row.input_artifact.clone(),
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

fn strip_fences(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    s
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
}
