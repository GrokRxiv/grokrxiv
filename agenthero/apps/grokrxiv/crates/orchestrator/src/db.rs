//! Database helpers for the `jobs`, `reviews`, and `uploads` tables.
//!
//! All functions accept a borrowed [`sqlx::PgPool`]. The repository boundary
//! uses typed row structs with runtime-checked SQLx queries so the crate still
//! builds before migrations are applied. Migration-driven `query!` macros can
//! replace these helpers later once the query surface is smaller.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agents::AgentRunnerKind;
use grokrxiv_schemas::{JobKind, JobState, PaperExtract, ReviewStatus, VerifierStatus};

const PAPER_REVIEW_DAG_ID: &str = "paper-review";

/// Insert a new row into `jobs` and return its id.
pub async fn create_job(pool: &PgPool, kind: JobKind, ref_id: Option<Uuid>) -> sqlx::Result<Uuid> {
    let kind_str = serde_plain(&kind);
    let state_str = serde_plain(&JobState::Queued);
    let id = Uuid::new_v4();
    sqlx::query("insert into jobs (id, kind, ref_id, state, attempt) values ($1, $2, $3, $4, 0)")
        .bind(id)
        .bind(kind_str)
        .bind(ref_id)
        .bind(state_str)
        .execute(pool)
        .await?;
    Ok(id)
}

/// Mark a job as `running` and bump its `started_at`.
pub async fn mark_running(pool: &PgPool, job_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "update jobs set state = 'running', started_at = now(), attempt = attempt + 1 where id = $1",
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a job done; clear error.
pub async fn mark_done(pool: &PgPool, job_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("update jobs set state = 'done', finished_at = now(), error = null where id = $1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark a job failed and persist the error message.
pub async fn mark_failed(pool: &PgPool, job_id: Uuid, err: &str) -> sqlx::Result<()> {
    sqlx::query("update jobs set state = 'failed', finished_at = now(), error = $2 where id = $1")
        .bind(job_id)
        .bind(err)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update a review's status. Used by the webhook handler when a PR merges.
/// Returns the number of rows updated so callers can gate side-effects (like
/// posting a revalidate call) on the DB actually transitioning.
pub async fn set_review_status(
    pool: &PgPool,
    review_id: Uuid,
    status: ReviewStatus,
    published_at: Option<DateTime<Utc>>,
) -> sqlx::Result<u64> {
    let s = serde_plain(&status);
    let res = sqlx::query("update reviews set status = $2, published_at = $3 where id = $1")
        .bind(review_id)
        .bind(s)
        .bind(published_at)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

pub(crate) async fn set_review_system_failed(
    pool: &PgPool,
    review_id: Uuid,
    failure_code: &str,
    failure_message: &str,
    failure_retryable: bool,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update reviews \
            set status = 'system_failed', \
                failure_code = $2, \
                failure_message = $3, \
                failure_retryable = $4, \
                failed_at = now() \
          where id = $1",
    )
    .bind(review_id)
    .bind(failure_code)
    .bind(failure_message)
    .bind(failure_retryable)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub(crate) async fn set_review_meta_review(
    pool: &PgPool,
    review_id: Uuid,
    meta_review: &Value,
) -> sqlx::Result<u64> {
    let res = sqlx::query("update reviews set meta_review = $2 where id = $1")
        .bind(review_id)
        .bind(meta_review)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

pub(crate) async fn set_review_github_pr_url(
    pool: &PgPool,
    review_id: Uuid,
    github_pr_url: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
        .bind(review_id)
        .bind(github_pr_url)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Insert a row into `uploads` for a fast-preview sample. Returns the inserted
/// row id.
pub async fn insert_sample_upload(
    pool: &PgPool,
    pdf_path: Option<String>,
    preview_review: Value,
    bundle_path: Option<String>,
    ip_hash: Option<String>,
) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into uploads (id, ip_hash, pdf_path, preview_review, bundle_path) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(ip_hash)
    .bind(pdf_path)
    .bind(preview_review)
    .bind(bundle_path)
    .execute(pool)
    .await?;
    Ok(id)
}

fn serde_plain<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) fn review_status_from_db_str(s: &str) -> Option<ReviewStatus> {
    match s {
        "draft" => Some(ReviewStatus::Draft),
        "awaiting_moderation" => Some(ReviewStatus::AwaitingModeration),
        "in_review" => Some(ReviewStatus::InReview),
        "pr_open" => Some(ReviewStatus::PrOpen),
        "published" => Some(ReviewStatus::Published),
        "corrected" => Some(ReviewStatus::Corrected),
        "withdrawn" => Some(ReviewStatus::Withdrawn),
        "rejected" => Some(ReviewStatus::Rejected),
        "system_failed" => Some(ReviewStatus::SystemFailed),
        _ => None,
    }
}

pub(crate) fn verifier_status_from_db_str(s: &str) -> Option<VerifierStatus> {
    match s {
        "pass" => Some(VerifierStatus::Pass),
        "warn" => Some(VerifierStatus::Warn),
        "fail" => Some(VerifierStatus::Fail),
        _ => None,
    }
}

pub(crate) fn agent_runner_from_db_str(s: &str) -> Option<AgentRunnerKind> {
    match s {
        "api" => Some(AgentRunnerKind::Api),
        "cli" => Some(AgentRunnerKind::Cli),
        _ => None,
    }
}

/// Load specialist verifier aggregate for the publication gate.
pub(crate) async fn load_specialist_gate_for_review(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<crate::review_gate::SpecialistGate> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "select role, verifier_status from review_agents \
         where review_id = $1 \
           and dag_type = coalesce((select dag_type from reviews where id = $1), 'paper-review') \
           and coalesce(node_kind, 'agent') = 'agent' \
         order by role",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await?;
    let statuses: Vec<(String, Option<VerifierStatus>)> = rows
        .into_iter()
        .map(|(role, status)| {
            (
                role,
                status.as_deref().and_then(verifier_status_from_db_str),
            )
        })
        .collect();
    let required_roles =
        crate::agents::config::dag_feeds_meta_roles(PAPER_REVIEW_DAG_ID).map_err(|err| {
            sqlx::Error::Protocol(format!(
                "load `{PAPER_REVIEW_DAG_ID}` specialist roles: {err:#}"
            ))
        })?;
    if required_roles.is_empty() {
        return Err(sqlx::Error::Protocol(format!(
            "`{PAPER_REVIEW_DAG_ID}` declares no feeds_meta specialist roles"
        )));
    }
    Ok(crate::review_gate::SpecialistGate::evaluate_required_roles(
        &required_roles,
        &statuses,
        3usize.min(required_roles.len()),
    ))
}

/// Public-safe source metadata persisted on `papers`.
#[derive(Debug, Clone, Default)]
pub struct PaperSourceMetadata {
    /// Broad source adapter that prepared the paper.
    pub source_kind: String,
    /// Stable source identifier. For arXiv this is the arXiv id; for local/git
    /// sources this is content-hash based.
    pub source_id: String,
    /// Canonical source URI when it is safe to persist.
    pub source_uri: Option<String>,
    /// SHA-256 or equivalent content hash for the manuscript input.
    pub source_hash: Option<String>,
    /// Adapter-specific source metadata, e.g. git commit/repo details.
    pub source_metadata: Value,
}

impl PaperSourceMetadata {
    /// Build the legacy arXiv metadata projection for existing call sites.
    pub fn arxiv(arxiv_id: &str) -> Self {
        Self {
            source_kind: "arxiv".to_string(),
            source_id: arxiv_id.to_string(),
            source_uri: Some(format!("https://arxiv.org/abs/{arxiv_id}")),
            source_hash: None,
            source_metadata: Value::Object(Default::default()),
        }
    }
}

// ---------------------------------------------------------------------------
// M1 persistence helpers — paper / review / review_agents
// ---------------------------------------------------------------------------

/// Insert or update a paper from a `PaperExtract`. Returns the row id.
/// Idempotent on the unique `arxiv_id`.
pub async fn upsert_paper(
    pool: &PgPool,
    extract: &PaperExtract,
    submitted_date: Option<NaiveDate>,
) -> sqlx::Result<Uuid> {
    upsert_paper_with_source(
        pool,
        extract,
        submitted_date,
        &PaperSourceMetadata::arxiv(&extract.arxiv_id),
    )
    .await
}

/// Insert or update a paper from a `PaperExtract` plus source identity.
/// Idempotent on `(source_kind, source_id)` while keeping `arxiv_id` populated
/// for compatibility with existing routes and artifacts.
pub async fn upsert_paper_with_source(
    pool: &PgPool,
    extract: &PaperExtract,
    submitted_date: Option<NaiveDate>,
    source: &PaperSourceMetadata,
) -> sqlx::Result<Uuid> {
    let authors_json =
        serde_json::to_value(&extract.authors).unwrap_or_else(|_| Value::Array(vec![]));
    let id: Uuid = sqlx::query_scalar(
        "insert into papers \
           (arxiv_id, title, authors, abstract, field, submitted_date, \
            source_kind, source_id, source_uri, source_hash, source_metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (source_kind, source_id) where source_id is not null do update set
           arxiv_id = excluded.arxiv_id,
           title = excluded.title,
           authors = excluded.authors,
           abstract = excluded.abstract,
           field = excluded.field,
           submitted_date = coalesce(excluded.submitted_date, papers.submitted_date),
           source_uri = excluded.source_uri,
           source_hash = excluded.source_hash,
           source_metadata = excluded.source_metadata
         returning id",
    )
    .bind(&extract.arxiv_id)
    .bind(&extract.title)
    .bind(authors_json)
    .bind(extract.abstract_.as_str())
    .bind(extract.field.as_deref())
    .bind(submitted_date)
    .bind(&source.source_kind)
    .bind(&source.source_id)
    .bind(source.source_uri.as_deref())
    .bind(source.source_hash.as_deref())
    .bind(&source.source_metadata)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Refresh the mutable source snapshot for an existing logical paper without
/// changing its stable `source_id`. Used by GitHub correction-loop re-reviews:
/// the author pushed a new commit for the same submission, so the paper row
/// should show the latest title/hash/metadata while existing review links keep
/// pointing at the same paper id.
pub async fn update_paper_source_snapshot(
    pool: &PgPool,
    paper_id: Uuid,
    extract: &PaperExtract,
    source: &PaperSourceMetadata,
) -> sqlx::Result<u64> {
    let authors_json =
        serde_json::to_value(&extract.authors).unwrap_or_else(|_| Value::Array(vec![]));
    let res = sqlx::query(
        "update papers set \
           title = $2, \
           authors = $3, \
           abstract = $4, \
           field = $5, \
           source_uri = $6, \
           source_hash = $7, \
           source_metadata = $8 \
         where id = $1",
    )
    .bind(paper_id)
    .bind(&extract.title)
    .bind(authors_json)
    .bind(extract.abstract_.as_str())
    .bind(extract.field.as_deref())
    .bind(source.source_uri.as_deref())
    .bind(source.source_hash.as_deref())
    .bind(&source.source_metadata)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Insert a new review row for `paper_id` at `awaiting_moderation`. Returns
/// the new review id.
///
/// Any pre-existing review for the same paper in a non-terminal status is
/// transitioned to `withdrawn` first, so re-reviewing a paper SUPERSEDES the
/// old review rather than creating a parallel one.
pub async fn insert_review(
    pool: &PgPool,
    paper_id: Uuid,
    models_used: Value,
    meta_review: Option<Value>,
) -> sqlx::Result<Uuid> {
    insert_review_with_submission(pool, paper_id, models_used, meta_review, None, "public").await
}

/// Insert a review row with submitter and visibility metadata from the account
/// workflow.
pub async fn insert_review_with_submission(
    pool: &PgPool,
    paper_id: Uuid,
    models_used: Value,
    meta_review: Option<Value>,
    submitted_by: Option<Uuid>,
    visibility: &str,
) -> sqlx::Result<Uuid> {
    let mut tx = pool.begin().await?;

    // Withdraw any active reviews for this paper. 'draft', 'in_review',
    // 'awaiting_moderation', 'pr_open', 'published', 'corrected' all count as
    // active and get superseded by the new run; 'withdrawn' rows are left as-is.
    //
    // FP-RPT3b B6: capture the ids so we can transition their moderation_queue
    // rows to `superseded` in the same transaction. Without this, the prior
    // moderation rows would be left pointing at a withdrawn review row,
    // making the moderator view inconsistent.
    let superseded: Vec<(Uuid,)> = sqlx::query_as(
        "update reviews \
         set status='withdrawn', superseded_at=now() \
         where paper_id=$1 \
           and status in ('draft','in_review','awaiting_moderation','pr_open','published','corrected') \
         returning id",
    )
    .bind(paper_id)
    .fetch_all(&mut *tx)
    .await?;

    if !superseded.is_empty() {
        let ids: Vec<Uuid> = superseded.iter().map(|(i,)| *i).collect();
        // FP-RPT3b B6: corresponding mq rows graduate to the new `superseded`
        // terminal state (migration 20260516000005). `rejected` rows are
        // intentionally left alone — a rejection that happens to be
        // superseded later keeps its rejection as the primary signal.
        sqlx::query(
            "update moderation_queue \
             set state='superseded' \
             where review_id = any($1) \
               and state in ('pending','approved','changes_requested')",
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }

    let id = Uuid::new_v4();
    let status = serde_plain(&ReviewStatus::AwaitingModeration);
    sqlx::query(
        "insert into reviews \
         (id, paper_id, status, models_used, meta_review, submitted_by, visibility) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(paper_id)
    .bind(status)
    .bind(models_used)
    .bind(meta_review)
    .bind(submitted_by)
    .bind(visibility)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(id)
}

/// Mark a user-submitted review job as running.
pub async fn mark_submission_running(
    pool: &PgPool,
    submission_id: Uuid,
    paper_id: Option<Uuid>,
) -> sqlx::Result<()> {
    sqlx::query("select public.grokrxiv_mark_submission_running($1, $2)")
        .bind(submission_id)
        .bind(paper_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Attach the completed review to its user-facing submission row.
pub async fn mark_submission_review_ready(
    pool: &PgPool,
    submission_id: Uuid,
    review_id: Uuid,
    paper_id: Uuid,
    visibility: &str,
) -> sqlx::Result<()> {
    sqlx::query("select public.grokrxiv_mark_submission_review_ready($1, $2, $3, $4)")
        .bind(submission_id)
        .bind(review_id)
        .bind(paper_id)
        .bind(visibility)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark a user-facing submission as failed and refund quota.
pub async fn mark_submission_failed(
    pool: &PgPool,
    submission_id: Uuid,
    error: &str,
) -> sqlx::Result<()> {
    sqlx::query("select public.grokrxiv_mark_submission_failed($1, $2, true)")
        .bind(submission_id)
        .bind(error)
        .execute(pool)
        .await?;
    Ok(())
}

/// Phase 3: fetch the latest moderator `--notes` recorded via
/// `agenthero grokrxiv request-changes` for any prior review of this paper. The
/// supervisor surfaces these notes to specialist + meta prompts on the next
/// review pass so the agents react to operator feedback. Returns `None` when
/// the paper has no `changes_requested` history.
pub async fn fetch_latest_changes_request_notes(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "select mq.notes \
         from moderation_queue mq \
         join reviews r on r.id = mq.review_id \
         where r.paper_id = $1 \
           and mq.state = 'changes_requested' \
           and mq.notes is not null \
         order by mq.created_at desc \
         limit 1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(n,)| n))
}

/// Look up the PR URL of the most recently superseded review for a paper, if
/// any. The publisher uses this to close the stale PR on `grokrxiv-reviews`
/// after the new review's PR is opened.
pub async fn fetch_superseded_pr_url(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "select r.github_pr_url \
         from reviews r \
         where r.paper_id = $1 \
           and r.status = 'withdrawn' \
           and r.superseded_at is not null \
           and r.github_pr_url is not null \
         order by r.superseded_at desc \
         limit 1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(url,)| url))
}

/// Persist a single specialist agent's output against a review.
///
/// The shared specialist input artifact is recorded ONCE per review in the
/// `review_inputs` table — see [`insert_review_input`]. The meta-reviewer's
/// input is just `{role -> output}` over the five specialists, which is
/// reconstructable from the persisted `review_agents.output` rows.
#[derive(Debug, Clone)]
pub struct ReviewAgentInsert<'a> {
    /// Review id owning this agent row.
    pub review_id: Uuid,
    /// DAG type owning this agent row.
    pub dag_type: String,
    /// DAG-scoped agent id. Legacy paper-review rows use the former role slug
    /// values such as `summary` and `meta_reviewer`.
    pub role: String,
    /// DAG node id that emitted this row. Defaults to `role` for legacy rows.
    pub node_id: Option<String>,
    /// Agent capability/type from the DAG manifest.
    pub agent_type: Option<String>,
    /// DAG node kind that emitted this row.
    pub node_kind: Option<String>,
    /// Runner backend that actually executed this role.
    pub runner: AgentRunnerKind,
    /// Model id used for the call.
    pub model: &'a str,
    /// Typed output artifact produced by the agent.
    pub output: Value,
    /// Aggregate verifier status.
    pub verifier_status: Option<VerifierStatus>,
    /// Full verifier rung notes.
    pub verifier_notes: Option<Value>,
    /// Prompt/input tokens when reported by the provider.
    pub tokens_in: Option<i32>,
    /// Completion/output tokens when reported by the provider.
    pub tokens_out: Option<i32>,
    /// Provider latency in milliseconds.
    pub latency_ms: Option<i32>,
}

/// Persist a single specialist agent's output against a review.
///
/// FP6 A2: the `input_artifact` column was dropped from `review_agents` in
/// migration `20260515000001_review_inputs.sql`; the per-review shared input
/// now lives on `review_inputs` and is written exactly once per review.
pub async fn insert_review_agent(pool: &PgPool, row: ReviewAgentInsert<'_>) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    let runner_str = serde_plain(&row.runner);
    let vstatus = row.verifier_status.as_ref().map(serde_plain);
    sqlx::query(
        "insert into review_agents \
           (id, review_id, dag_type, role, node_id, agent_type, node_kind, runner, model, output, \
            verifier_status, verifier_notes, tokens_in, tokens_out, latency_ms) \
         values ($1, $2, $3, $4, coalesce($5, $4), coalesce($6, 'critic'), coalesce($7, 'agent'), \
                 $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(id)
    .bind(row.review_id)
    .bind(&row.dag_type)
    .bind(&row.role)
    .bind(row.node_id)
    .bind(row.agent_type)
    .bind(row.node_kind)
    .bind(runner_str)
    .bind(row.model)
    .bind(row.output)
    .bind(vstatus)
    .bind(row.verifier_notes)
    .bind(row.tokens_in)
    .bind(row.tokens_out)
    .bind(row.latency_ms)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Insert the shared specialist input artifact for a review. Called exactly
/// once per review at the start of the DAG, replacing the previous behaviour
/// of stamping the same JSONB onto every `review_agents` row.
pub async fn insert_review_input(
    pool: &PgPool,
    review_id: Uuid,
    paper_id: Uuid,
    artifact: &Value,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into review_inputs (review_id, paper_id, dag_type, artifact) \
         values ($1, $2, coalesce((select dag_type from reviews where id = $1), 'paper-review'), $3) \
         on conflict (review_id) do update set \
           dag_type = excluded.dag_type, \
           artifact = excluded.artifact",
    )
    .bind(review_id)
    .bind(paper_id)
    .bind(artifact)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the shared specialist input artifact for a review, if any.
pub async fn load_review_input(pool: &PgPool, review_id: Uuid) -> sqlx::Result<Option<Value>> {
    let row: Option<(Value,)> =
        sqlx::query_as("select artifact from review_inputs where review_id = $1")
            .bind(review_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}

// ---------------------------------------------------------------------------
// FP6 A4: per-paper output cache
// ---------------------------------------------------------------------------

/// A cached agent output for a `(paper_id, role, content_hash)` triple.
#[derive(Debug, Clone)]
pub struct CachedOutput {
    /// The cached output JSON the agent originally produced.
    pub output: Value,
    /// Verifier verdict at cache time. Only `pass` rows are cached.
    pub verifier_status: String,
    /// Model id that produced the cached output.
    pub model: String,
    /// Runner backend that produced the cached output.
    pub runner: AgentRunnerKind,
    /// Prompt tokens the original call consumed.
    pub tokens_in: Option<i32>,
    /// Completion tokens the original call produced.
    pub tokens_out: Option<i32>,
}

/// Look up a non-expired cache row for `(paper_id, role, content_hash)`. The
/// uniqueness index guarantees at most one matching row.
pub async fn lookup_cache(
    pool: &PgPool,
    paper_id: Uuid,
    dag_type: &str,
    role: &str,
    content_hash: &str,
) -> sqlx::Result<Option<CachedOutput>> {
    let row: Option<(Value, String, String, String, Option<i32>, Option<i32>)> = sqlx::query_as(
        "select output, verifier_status, model, runner, tokens_in, tokens_out \
         from review_cache \
         where paper_id = $1 and dag_type = $2 and role = $3 and content_hash = $4 \
           and expires_at > now() \
         limit 1",
    )
    .bind(paper_id)
    .bind(dag_type)
    .bind(role)
    .bind(content_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(output, verifier_status, model, runner, tokens_in, tokens_out)| CachedOutput {
            output,
            verifier_status,
            model,
            runner: agent_runner_from_db_str(&runner).unwrap_or(AgentRunnerKind::Api),
            tokens_in,
            tokens_out,
        },
    ))
}

/// Insert (or refresh) a cache row for a successful agent call. The unique
/// `(paper_id, role, content_hash)` index makes this an idempotent upsert.
#[allow(clippy::too_many_arguments)]
pub async fn insert_cache(
    pool: &PgPool,
    paper_id: Uuid,
    dag_type: &str,
    role: &str,
    content_hash: &str,
    output: &Value,
    verifier_status: &str,
    model: &str,
    runner: AgentRunnerKind,
    tokens_in: Option<i32>,
    tokens_out: Option<i32>,
) -> sqlx::Result<()> {
    let runner_str = serde_plain(&runner);
    sqlx::query(
        "insert into review_cache \
           (paper_id, dag_type, role, content_hash, output, verifier_status, model, runner, tokens_in, tokens_out) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         on conflict (dag_type, paper_id, role, content_hash) do update set \
           output = excluded.output, \
           verifier_status = excluded.verifier_status, \
           model = excluded.model, \
           runner = excluded.runner, \
           tokens_in = excluded.tokens_in, \
           tokens_out = excluded.tokens_out, \
           created_at = now(), \
           expires_at = now() + interval '30 days'",
    )
    .bind(paper_id)
    .bind(dag_type)
    .bind(role)
    .bind(content_hash)
    .bind(output)
    .bind(verifier_status)
    .bind(model)
    .bind(runner_str)
    .bind(tokens_in)
    .bind(tokens_out)
    .execute(pool)
    .await?;
    Ok(())
}

/// Stash render output paths on the review row once render completes.
pub async fn set_review_artifacts(
    pool: &PgPool,
    review_id: Uuid,
    html_path: Option<&str>,
    pdf_path: Option<&str>,
    zip_path: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("update reviews set html_path = $2, pdf_path = $3, zip_path = $4 where id = $1")
        .bind(review_id)
        .bind(html_path)
        .bind(pdf_path)
        .bind(zip_path)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// FP4: moderation lifecycle helpers
// ---------------------------------------------------------------------------

/// Insert a `pending` row into `moderation_queue` for a freshly-landed review.
/// Returns the new moderation row id. Call this after the review DAG has
/// completed successfully; system-failed reviews must not enter moderation.
pub async fn insert_moderation_pending(pool: &PgPool, review_id: Uuid) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into moderation_queue (id, review_id, dag_type, state) \
         values ($1, $2, coalesce((select dag_type from reviews where id = $2), 'paper-review'), 'pending')",
    )
    .bind(id)
    .bind(review_id)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Update the most-recent moderation row for `review_id` (matched by
/// `created_at desc`). Returns the number of rows updated so callers can fail
/// fast when no matching row exists.
///
/// `state` must be one of the values permitted by the `moderation_queue.state`
/// check constraint: `pending`, `approved`, `rejected`, `changes_requested`.
pub async fn update_moderation_state(
    pool: &PgPool,
    review_id: Uuid,
    state: &str,
    notes: Option<&str>,
    moderator: Option<&str>,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update moderation_queue \
         set state = $2, notes = coalesce($3, notes), \
             moderator = coalesce($4, moderator), decided_at = now() \
         where id = ( \
           select id from moderation_queue \
           where review_id = $1 \
           order by created_at desc \
           limit 1)",
    )
    .bind(review_id)
    .bind(state)
    .bind(notes)
    .bind(moderator)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Append a row to `corrections` for `review_id`. `kind` must be one of
/// `correction`, `withdrawal`, `clarification`.
pub async fn insert_correction(
    pool: &PgPool,
    review_id: Uuid,
    kind: &str,
    rationale_md: &str,
    created_by: &str,
) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into corrections (id, review_id, kind, rationale_md, created_by) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(review_id)
    .bind(kind)
    .bind(rationale_md)
    .bind(created_by)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Insert a durable lifecycle event. `github_delivery_id` is unique when set
/// and lets webhook handlers acknowledge duplicate deliveries without
/// repeating side effects.
pub async fn insert_review_event(
    pool: &PgPool,
    review_id: Option<Uuid>,
    paper_id: Option<Uuid>,
    event_type: &str,
    source: &str,
    payload: &Value,
    github_delivery_id: Option<&str>,
) -> sqlx::Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let res = sqlx::query(
        "insert into review_events \
           (id, review_id, paper_id, event_type, source, payload, github_delivery_id) \
         values ($1, $2, $3, $4, $5, $6, $7) \
         on conflict (github_delivery_id) where github_delivery_id is not null do nothing",
    )
    .bind(id)
    .bind(review_id)
    .bind(paper_id)
    .bind(event_type)
    .bind(source)
    .bind(payload)
    .bind(github_delivery_id)
    .execute(pool)
    .await?;
    Ok((res.rows_affected() > 0).then_some(id))
}

/// Persist a structured automated gate failure for a review.
pub async fn insert_review_gate_failure(
    pool: &PgPool,
    review_id: Uuid,
    gate: &str,
    severity: &str,
    summary: &str,
    details_md: &str,
    action_required_md: Option<&str>,
) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "update review_gate_failures \
         set status = 'superseded', resolved_at = now() \
         where review_id = $1 and gate = $2 and status = 'open'",
    )
    .bind(review_id)
    .bind(gate)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into review_gate_failures \
           (id, review_id, gate, severity, summary, details_md, action_required_md) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(review_id)
    .bind(gate)
    .bind(severity)
    .bind(summary)
    .bind(details_md)
    .bind(action_required_md)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Attach a GitHub feedback comment URL to every open gate failure for a
/// review. The publisher keeps exactly one stable comment per review.
pub async fn attach_gate_feedback_comment(
    pool: &PgPool,
    review_id: Uuid,
    comment_id: i64,
    comment_url: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update review_gate_failures \
         set github_comment_id = $2, github_comment_url = $3 \
         where review_id = $1 and status = 'open'",
    )
    .bind(review_id)
    .bind(comment_id)
    .bind(comment_url)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Upsert the GitHub publication/review thread associated with a review PR.
pub async fn upsert_github_review_thread(
    pool: &PgPool,
    review_id: Uuid,
    paper_id: Uuid,
    repo_owner: &str,
    repo_name: &str,
    pr_number: Option<i64>,
    pr_url: Option<&str>,
    head_ref: Option<&str>,
    head_sha: Option<&str>,
) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query_scalar(
        "insert into github_review_threads \
           (id, review_id, paper_id, repo_owner, repo_name, pr_number, pr_url, head_ref, head_sha, last_seen_commit_sha) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
         on conflict (review_id) do update set \
           paper_id = excluded.paper_id, \
           repo_owner = excluded.repo_owner, \
           repo_name = excluded.repo_name, \
           pr_number = coalesce(excluded.pr_number, github_review_threads.pr_number), \
           pr_url = coalesce(excluded.pr_url, github_review_threads.pr_url), \
           head_ref = coalesce(excluded.head_ref, github_review_threads.head_ref), \
           head_sha = coalesce(excluded.head_sha, github_review_threads.head_sha), \
           last_seen_commit_sha = coalesce(excluded.last_seen_commit_sha, github_review_threads.last_seen_commit_sha), \
           updated_at = now() \
         returning id",
    )
    .bind(id)
    .bind(review_id)
    .bind(paper_id)
    .bind(repo_owner)
    .bind(repo_name)
    .bind(pr_number)
    .bind(pr_url)
    .bind(head_ref)
    .bind(head_sha)
    .fetch_one(pool)
    .await
}

/// Persist feedback-comment metadata on the GitHub review thread.
pub async fn update_github_feedback_comment(
    pool: &PgPool,
    review_id: Uuid,
    comment_id: i64,
    comment_url: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update github_review_threads \
         set feedback_comment_id = $2, feedback_comment_url = $3, updated_at = now() \
         where review_id = $1",
    )
    .bind(review_id)
    .bind(comment_id)
    .bind(comment_url)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

#[derive(Debug, Clone)]
pub(crate) struct FeedbackLoopThread {
    pub paper_id: Uuid,
    pub github_pr_url: Option<String>,
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub pr_number: Option<i64>,
    pub feedback_comment_url: Option<String>,
}

pub(crate) async fn fetch_feedback_loop_thread(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<Option<FeedbackLoopThread>> {
    sqlx::query_as(
        "select r.paper_id, r.github_pr_url, t.repo_owner, t.repo_name, t.pr_number, t.feedback_comment_url \
         from reviews r \
         left join github_review_threads t on t.review_id = r.id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await
    .map(|row: Option<(Uuid, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>)>| {
        row.map(
            |(paper_id, github_pr_url, repo_owner, repo_name, pr_number, feedback_comment_url)| {
                FeedbackLoopThread {
                    paper_id,
                    github_pr_url,
                    repo_owner,
                    repo_name,
                    pr_number,
                    feedback_comment_url,
                }
            },
        )
    })
}

#[derive(Debug, Clone)]
pub(crate) struct RereviewRequestStatus {
    pub id: Uuid,
    pub state: String,
    pub new_review_id: Option<Uuid>,
    pub error: Option<String>,
}

pub(crate) async fn fetch_rereview_request_for_commit(
    pool: &PgPool,
    prior_review_id: Uuid,
    github_commit_sha: &str,
) -> sqlx::Result<Option<RereviewRequestStatus>> {
    sqlx::query_as(
        "select id, state, new_review_id, error \
         from rereview_requests \
         where prior_review_id = $1 and github_commit_sha = $2 \
         order by created_at desc \
         limit 1",
    )
    .bind(prior_review_id)
    .bind(github_commit_sha)
    .fetch_optional(pool)
    .await
    .map(
        |row: Option<(Uuid, String, Option<Uuid>, Option<String>)>| {
            row.map(|(id, state, new_review_id, error)| RereviewRequestStatus {
                id,
                state,
                new_review_id,
                error,
            })
        },
    )
}

/// Enqueue a re-review request triggered by a new author correction commit.
/// Duplicate `(prior_review_id, sha)` requests are ignored idempotently.
pub async fn enqueue_rereview_for_commit(
    pool: &PgPool,
    paper_id: Uuid,
    prior_review_id: Uuid,
    github_commit_sha: &str,
    requested_by: Option<&str>,
    notes_md: Option<&str>,
) -> sqlx::Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let res = sqlx::query(
        "insert into rereview_requests \
           (id, paper_id, prior_review_id, trigger, github_commit_sha, requested_by, notes_md) \
         values ($1, $2, $3, 'author_commit', $4, $5, $6) \
         on conflict (prior_review_id, github_commit_sha) where github_commit_sha is not null do nothing",
    )
    .bind(id)
    .bind(paper_id)
    .bind(prior_review_id)
    .bind(github_commit_sha)
    .bind(requested_by)
    .bind(notes_md)
    .execute(pool)
    .await?;
    Ok((res.rows_affected() > 0).then_some(id))
}

/// Mark a queued re-review request as running.
pub async fn mark_rereview_running(pool: &PgPool, request_id: Uuid) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update rereview_requests \
         set state = 'running', started_at = now(), error = null \
         where id = $1 and state = 'queued'",
    )
    .bind(request_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Mark a re-review request complete and attach the newly created review id.
pub async fn mark_rereview_done(
    pool: &PgPool,
    request_id: Uuid,
    new_review_id: Uuid,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update rereview_requests \
         set state = 'done', new_review_id = $2, finished_at = now(), error = null \
         where id = $1",
    )
    .bind(request_id)
    .bind(new_review_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Mark a re-review request failed with a human-readable error string.
pub async fn mark_rereview_failed(
    pool: &PgPool,
    request_id: Uuid,
    error: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update rereview_requests \
         set state = 'failed', finished_at = now(), error = $2 \
         where id = $1",
    )
    .bind(request_id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// One row in the `list reviews` CLI output.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ReviewListRow {
    /// Review row id.
    pub id: Uuid,
    /// Paper row id.
    pub paper_id: Uuid,
    /// Review lifecycle status.
    pub status: String,
    /// arXiv id of the paper.
    pub arxiv_id: String,
    /// Paper title.
    pub title: String,
    /// When the review row was created.
    pub created_at: DateTime<Utc>,
}

/// List the most recent reviews, optionally filtered by status.
pub async fn list_reviews(
    pool: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<ReviewListRow>> {
    let limit = limit.clamp(1, 1000);
    let rows: Vec<ReviewListRow> = if let Some(s) = status {
        sqlx::query_as(
            "select r.id, r.paper_id, r.status, p.arxiv_id, p.title, r.created_at \
             from reviews r join papers p on p.id = r.paper_id \
             where r.status = $1 \
             order by r.created_at desc limit $2",
        )
        .bind(s)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "select r.id, r.paper_id, r.status, p.arxiv_id, p.title, r.created_at \
             from reviews r join papers p on p.id = r.paper_id \
             order by r.created_at desc limit $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?
    };
    Ok(rows)
}

/// Detailed view of a single review. Joins the paper and counts the
/// agent + correction rows so the CLI can pretty-print without N+1 queries.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ReviewDetailRow {
    /// Review row id.
    pub id: Uuid,
    /// Paper row id.
    pub paper_id: Uuid,
    /// Review lifecycle status.
    pub status: String,
    /// PR URL once `pr_open`/`published`.
    pub github_pr_url: Option<String>,
    /// arXiv id of the paper.
    pub arxiv_id: String,
    /// Paper title.
    pub title: String,
    /// Synthesized meta-review JSON.
    pub meta_review: Option<Value>,
    /// Number of review_agents rows persisted.
    pub agents_count: i64,
    /// Number of corrections rows persisted.
    pub corrections_count: i64,
    /// When the review row was created.
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RPT2 Track F: revision_patches CRUD
// ---------------------------------------------------------------------------

/// A persisted row in `revision_patches`. The supervisor's `apply_revisions`
/// function reads these to materialise a draft PR with per-patch
/// accept/reject checkboxes.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct RevisionPatchRow {
    /// `revision_patches.id` (bigserial).
    pub id: i64,
    /// Review the artifact belongs to.
    pub review_id: Uuid,
    /// `review_agents.id` that emitted the artifact.
    pub review_agent_id: Uuid,
    /// `paper_latex` | `grokrxiv_review_output`.
    pub target: String,
    /// JSONB array of patch objects (see `schemas/revision_artifact.schema.json`).
    pub patches: Value,
    /// Per-patch indices the moderator has accepted (empty until applied).
    pub accepted_indices: Vec<i32>,
    /// URL of the draft PR once `apply_revisions` has opened it.
    pub applied_pr_url: Option<String>,
    /// When the row was inserted.
    pub created_at: DateTime<Utc>,
    /// When the patches were applied (PR opened); null until then.
    pub applied_at: Option<DateTime<Utc>>,
}

/// Insert a revision_artifact for a single agent. The agent emitted
/// `patches` for the given `target` (paper_latex or grokrxiv_review_output)
/// while running in `AgentMode::ReviewAndRevise`. Returns the inserted row's
/// id so the caller can attach it to in-memory bookkeeping.
pub async fn insert_revision_patches(
    pool: &PgPool,
    review_id: Uuid,
    review_agent_id: Uuid,
    target: &str,
    patches: &Value,
) -> sqlx::Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "insert into revision_patches \
           (review_id, review_agent_id, target, patches) \
         values ($1, $2, $3, $4) \
         returning id",
    )
    .bind(review_id)
    .bind(review_agent_id)
    .bind(target)
    .bind(patches)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// List every revision_patches row belonging to `review_id`, oldest first.
/// Used by `apply_revisions` to materialise the draft PR.
pub async fn list_revision_patches(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<Vec<RevisionPatchRow>> {
    let rows: Vec<RevisionPatchRow> = sqlx::query_as(
        "select id, review_id, review_agent_id, target, patches, \
                accepted_indices, applied_pr_url, created_at, applied_at \
         from revision_patches \
         where review_id = $1 \
         order by created_at asc, id asc",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Stamp the accepted indices and (optionally) the PR URL onto a
/// `revision_patches` row. When `applied_pr_url` is `Some`, the row is also
/// marked as applied via `applied_at = now()`. Returns the number of rows
/// affected so callers can fail fast on an unknown id.
pub async fn update_revision_patches_accepted(
    pool: &PgPool,
    id: i64,
    accepted_indices: &[i32],
    applied_pr_url: Option<&str>,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "update revision_patches \
         set accepted_indices = $2, \
             applied_pr_url = coalesce($3, applied_pr_url), \
             applied_at = case when $3 is not null then now() else applied_at end \
         where id = $1",
    )
    .bind(id)
    .bind(accepted_indices)
    .bind(applied_pr_url)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Set the `reviews.mode` column. Idempotent. Returns the number of rows
/// affected so callers can detect a missing review id.
pub async fn set_review_mode(pool: &PgPool, review_id: Uuid, mode: &str) -> sqlx::Result<u64> {
    let res = sqlx::query("update reviews set mode = $2 where id = $1")
        .bind(review_id)
        .bind(mode)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Read the lifecycle status + mode of a review in one round-trip. Used by
/// `apply_revisions` to gate on the moderator-approved states without
/// pulling the full ReviewDetailRow shape.
pub async fn get_review_status_and_mode(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<Option<(String, String)>> {
    let row: Option<(String, String)> =
        sqlx::query_as("select status, mode from reviews where id = $1")
            .bind(review_id)
            .fetch_optional(pool)
            .await?;
    if let Some((status, _)) = row.as_ref() {
        debug_assert!(
            review_status_from_db_str(status).is_some(),
            "unknown reviews.status value: {status}"
        );
    }
    Ok(row)
}

/// Load a review + paper + counts in one go for the `show` CLI.
pub async fn show_review(pool: &PgPool, review_id: Uuid) -> sqlx::Result<Option<ReviewDetailRow>> {
    let row: Option<ReviewDetailRow> = sqlx::query_as(
        "select r.id, r.paper_id, r.status, r.github_pr_url, p.arxiv_id, p.title, \
                r.meta_review, \
                (select count(*) from review_agents a where a.review_id = r.id) as agents_count, \
                (select count(*) from corrections c where c.review_id = r.id) as corrections_count, \
                r.created_at \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub(crate) async fn list_pr_open_reviews_with_urls(
    pool: &PgPool,
    limit: i64,
) -> sqlx::Result<Vec<(Uuid, String)>> {
    sqlx::query_as(
        "select id, github_pr_url \
         from reviews \
         where status = 'pr_open' \
           and github_pr_url is not null \
           and github_pr_url not like '%SIMULATED-%' \
         order by created_at asc \
         limit $1",
    )
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PaperReviewSeedRow {
    pub(crate) arxiv_id: String,
    pub(crate) title: String,
    pub(crate) abstract_: Option<String>,
    pub(crate) field: Option<String>,
    pub(crate) submitted_date: Option<NaiveDate>,
}

pub(crate) async fn load_paper_review_seed(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<PaperReviewSeedRow> {
    sqlx::query_as(
        "select arxiv_id, title, abstract as abstract_, field, submitted_date \
         from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_one(pool)
    .await
}

pub(crate) async fn load_latest_review_input_artifact(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<Option<Value>> {
    let row: Option<(Value,)> = sqlx::query_as(
        "select ri.artifact \
         from review_inputs ri \
         join reviews r on r.id = ri.review_id \
         where ri.paper_id = $1 \
         order by r.created_at desc \
         limit 1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(artifact,)| artifact))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ReviewRenderHeadRow {
    pub(crate) meta_review: Option<Value>,
    pub(crate) paper_id: Uuid,
    pub(crate) arxiv_id: String,
    pub(crate) title: String,
    pub(crate) abstract_: Option<String>,
    pub(crate) field: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ReviewAgentRenderRow {
    pub(crate) role: String,
    pub(crate) model: String,
    pub(crate) output: Value,
    pub(crate) verifier_status: Option<String>,
    pub(crate) verifier_notes: Option<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewRenderBundle {
    pub(crate) review: ReviewRenderHeadRow,
    pub(crate) agents: Vec<ReviewAgentRenderRow>,
}

pub(crate) async fn load_review_render_bundle(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<ReviewRenderBundle> {
    let review: ReviewRenderHeadRow = sqlx::query_as(
        "select r.meta_review, p.id as paper_id, p.arxiv_id, p.title, \
                p.abstract as abstract_, p.field \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await?;

    let agents: Vec<ReviewAgentRenderRow> = sqlx::query_as(
        "select role, model, output, verifier_status, verifier_notes \
         from review_agents where review_id = $1 order by role",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await?;

    Ok(ReviewRenderBundle { review, agents })
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PublishReviewRow {
    pub(crate) review_id: Uuid,
    pub(crate) status: String,
    pub(crate) github_pr_url: Option<String>,
    pub(crate) arxiv_id: String,
    pub(crate) title: String,
    pub(crate) field: Option<String>,
    pub(crate) paper_id: Uuid,
    pub(crate) visibility: String,
    pub(crate) source_kind: String,
    pub(crate) source_id: Option<String>,
}

pub(crate) async fn load_publish_review(
    pool: &PgPool,
    review_id: Uuid,
) -> sqlx::Result<PublishReviewRow> {
    sqlx::query_as(
        "select r.id as review_id, r.status, r.github_pr_url, p.arxiv_id, p.title, p.field, p.id as paper_id, \
                coalesce(r.visibility, 'public') as visibility, \
                coalesce(p.source_kind, 'arxiv') as source_kind, p.source_id \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
}

// ---------------------------------------------------------------------------
// RPT3 Wave-3 Team-F: paper_assets extraction pipeline pointers
// ---------------------------------------------------------------------------

/// Current state of a paper's extraction pipeline. Mirrors the
/// `paper_assets.extraction_status` check constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionStatus {
    /// No extraction has run yet (the default for a fresh `paper_assets` row).
    Pending,
    /// An ingest run is currently in flight against this paper.
    Running,
    /// The pipeline finished successfully and `git_path` + `storage_prefix`
    /// are populated.
    Ready,
    /// The last run failed; the orchestrator may retry.
    Failed,
}

impl ExtractionStatus {
    /// Parse the string value stored in `paper_assets.extraction_status`.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
    /// Stringified form that round-trips through the DB.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// One row in the `paper_assets` table after a successful extraction.
#[derive(Debug, Clone)]
pub struct PaperAssetsRow {
    /// Owning paper's UUID.
    pub paper_id: Uuid,
    /// Tier-1 path under `grokrxiv-data` (e.g. `papers/2605.00403`). None
    /// until the first successful extraction.
    pub git_path: Option<String>,
    /// Commit SHA the Tier-1 artifacts were committed under. None when the
    /// data repo is operating in dry-run / no-commit mode.
    pub git_commit_sha: Option<String>,
    /// Tier-2 key prefix (typically just `<arxiv_id>`). None until the first
    /// successful extraction.
    pub storage_prefix: Option<String>,
    /// Pipeline state. Always present (defaults to `'pending'`).
    pub extraction_status: ExtractionStatus,
    /// Best-effort sum of per-stage USD cost.
    pub extraction_cost_usd: Option<f64>,
}

/// Read the current `paper_assets` pointer row for a paper. Returns `None`
/// when no row exists yet (the paper was inserted but the pipeline never
/// ran).
pub async fn read_paper_assets(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<Option<PaperAssetsRow>> {
    // NUMERIC values are cast to FLOAT8 in SQL so we can fetch them as f64
    // without pulling in the `sqlx::types::bigdecimal` feature flag.
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<f64>,
    )> = sqlx::query_as(
        "select git_path, git_commit_sha, storage_prefix, extraction_status, \
                extraction_cost_usd::float8 \
         from paper_assets where paper_id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(git_path, git_commit_sha, storage_prefix, extraction_status, cost)| PaperAssetsRow {
            paper_id,
            git_path,
            git_commit_sha,
            storage_prefix,
            extraction_status: ExtractionStatus::from_db_str(&extraction_status),
            extraction_cost_usd: cost,
        },
    ))
}

/// Insert (or upsert) a `paper_assets` row in the `running` state. The
/// orchestrator calls this at the start of Stage 8 so concurrent ingests can
/// see "another run is in flight" via [`read_paper_assets`].
pub async fn mark_paper_extracting(pool: &PgPool, paper_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "insert into paper_assets (paper_id, extraction_status) values ($1, 'running') \
         on conflict (paper_id) do update set extraction_status = 'running', updated_at = now()",
    )
    .bind(paper_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Finalise a `paper_assets` row after a successful extraction. Writes the
/// Tier-1 path + commit SHA, Tier-2 prefix, cost, and flips status to
/// `'ready'`. Idempotent on `paper_id`.
pub async fn persist_paper_extraction(
    pool: &PgPool,
    paper_id: Uuid,
    git_path: &str,
    git_commit_sha: Option<&str>,
    storage_prefix: &str,
    cost_usd: Option<f64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into paper_assets \
           (paper_id, git_path, git_commit_sha, storage_prefix, extraction_status, extraction_cost_usd) \
         values ($1, $2, $3, $4, 'ready', $5::float8::numeric) \
         on conflict (paper_id) do update set \
           git_path = excluded.git_path, \
           git_commit_sha = excluded.git_commit_sha, \
           storage_prefix = excluded.storage_prefix, \
           extraction_status = 'ready', \
           extraction_cost_usd = excluded.extraction_cost_usd, \
           updated_at = now()",
    )
    .bind(paper_id)
    .bind(git_path)
    .bind(git_commit_sha)
    .bind(storage_prefix)
    .bind(cost_usd)
    .execute(pool)
    .await?;
    Ok(())
}

/// Flip `paper_assets.extraction_status` to `'failed'` when a Stage 8 (or
/// earlier) step blew up. The next run will see `Failed` and retry. The
/// supplied `reason` is logged but not currently persisted — the
/// `extraction_report.json` carries the structured warnings, and adding a
/// `last_error` text column is out of scope for RPT3.
pub async fn mark_paper_extraction_failed(
    pool: &PgPool,
    paper_id: Uuid,
    reason: &str,
) -> sqlx::Result<()> {
    tracing::warn!(%paper_id, %reason, "extraction pipeline failed");
    sqlx::query(
        "insert into paper_assets (paper_id, extraction_status) values ($1, 'failed') \
         on conflict (paper_id) do update set extraction_status = 'failed', updated_at = now()",
    )
    .bind(paper_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_status_decoders_round_trip_known_values_and_reject_unknowns() {
        assert_eq!(
            review_status_from_db_str(&serde_plain(&ReviewStatus::AwaitingModeration)),
            Some(ReviewStatus::AwaitingModeration)
        );
        assert_eq!(
            review_status_from_db_str(&serde_plain(&ReviewStatus::PrOpen)),
            Some(ReviewStatus::PrOpen)
        );
        assert_eq!(
            review_status_from_db_str(&serde_plain(&ReviewStatus::SystemFailed)),
            Some(ReviewStatus::SystemFailed)
        );
        assert_eq!(
            verifier_status_from_db_str(&serde_plain(&VerifierStatus::Pass)),
            Some(VerifierStatus::Pass)
        );
        assert_eq!(
            verifier_status_from_db_str(&serde_plain(&VerifierStatus::Warn)),
            Some(VerifierStatus::Warn)
        );
        assert_eq!(review_status_from_db_str("half_published"), None);
        assert_eq!(verifier_status_from_db_str("maybe"), None);
    }

    #[test]
    fn agent_runner_decoders_round_trip_known_values_and_reject_unknowns() {
        assert_eq!(
            agent_runner_from_db_str(&serde_plain(&AgentRunnerKind::Api)),
            Some(AgentRunnerKind::Api)
        );
        assert_eq!(
            agent_runner_from_db_str(&serde_plain(&AgentRunnerKind::Cli)),
            Some(AgentRunnerKind::Cli)
        );
        assert_eq!(agent_runner_from_db_str("cloud"), None);
        assert_eq!(agent_runner_from_db_str("local_inference"), None);
        assert_eq!(agent_runner_from_db_str("api_fallback"), None);
    }

    #[test]
    fn review_agent_insert_accepts_custom_agent_id() {
        let row = ReviewAgentInsert {
            review_id: Uuid::nil(),
            dag_type: "paper-review".to_string(),
            role: "type_theory_validator".to_string(),
            node_id: Some("type_theory_validator".to_string()),
            agent_type: Some("type_theory_validator".to_string()),
            node_kind: Some("agent".to_string()),
            runner: AgentRunnerKind::Cli,
            model: "gpt-5.5",
            output: serde_json::json!({ "status": "ok" }),
            verifier_status: Some(VerifierStatus::Pass),
            verifier_notes: None,
            tokens_in: None,
            tokens_out: None,
            latency_ms: None,
        };

        assert_eq!(row.role, "type_theory_validator");
        assert_eq!(row.dag_type, "paper-review");
        assert_eq!(row.node_id.as_deref(), Some("type_theory_validator"));
        assert_eq!(row.agent_type.as_deref(), Some("type_theory_validator"));
        assert_eq!(row.node_kind.as_deref(), Some("agent"));
    }

    /// FP-RPT3b B6: when a fresh review supersedes a prior active review,
    /// the prior review's `moderation_queue` row must transition from
    /// `pending` to `superseded` in the same transaction. Without this
    /// the moderator view ends up with mq rows pointing at withdrawn
    /// reviews.
    ///
    /// Gated on `DATABASE_URL` so cargo test in CI without a DB doesn't
    /// fail. Run locally via:
    ///   DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:54322/postgres \
    ///     cargo test -p agenthero-orchestrator --features full --lib \
    ///     -- supersede_marks_prior_moderation_queue
    #[tokio::test]
    async fn supersede_marks_prior_moderation_queue_row_as_superseded() {
        let Ok(db_url) = std::env::var("DATABASE_URL") else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let pool = PgPool::connect(&db_url).await.expect("connect to test DB");

        // 1. Insert a fresh paper row to scope the test data.
        let arxiv_id = format!("fp-rpt3b-b6-test-{}", Uuid::new_v4());
        let paper_id: Uuid = sqlx::query_scalar(
            "insert into papers (arxiv_id, title, authors, abstract, field) \
             values ($1, $2, '[]'::jsonb, $3, $4) returning id",
        )
        .bind(&arxiv_id)
        .bind("FP-RPT3b B6 supersede test paper")
        .bind("placeholder abstract")
        .bind("cs.LG")
        .fetch_one(&pool)
        .await
        .expect("insert paper");

        // 2. First review: insert + add a pending moderation row.
        let first = insert_review(&pool, paper_id, serde_json::json!({}), None)
            .await
            .expect("first insert_review");
        insert_moderation_pending(&pool, first)
            .await
            .expect("insert mq pending");
        sqlx::query("update reviews set github_pr_url = $2 where id = $1")
            .bind(first)
            .bind("https://github.com/GrokRxiv/grokrxiv-reviews/pull/12345")
            .execute(&pool)
            .await
            .expect("set first github_pr_url");

        let first_state: (String,) =
            sqlx::query_as("select state from moderation_queue where review_id = $1")
                .bind(first)
                .fetch_one(&pool)
                .await
                .expect("read first mq state");
        assert_eq!(first_state.0, "pending");

        // 3. Second review for the same paper should auto-supersede the
        //    first AND transition its mq row.
        let second = insert_review(&pool, paper_id, serde_json::json!({}), None)
            .await
            .expect("second insert_review");
        assert_ne!(first, second);

        let first_state_after: (String,) =
            sqlx::query_as("select state from moderation_queue where review_id = $1")
                .bind(first)
                .fetch_one(&pool)
                .await
                .expect("read first mq state after supersede");
        assert_eq!(
            first_state_after.0, "superseded",
            "prior moderation_queue row must transition to 'superseded' after supersede"
        );

        let first_status: (String,) = sqlx::query_as("select status from reviews where id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .expect("read first review status");
        assert_eq!(first_status.0, "withdrawn");

        let superseded_pr = fetch_superseded_pr_url(&pool, paper_id)
            .await
            .expect("fetch superseded pr url");
        assert_eq!(
            superseded_pr.as_deref(),
            Some("https://github.com/GrokRxiv/grokrxiv-reviews/pull/12345")
        );

        // 4. Clean up: cascade delete via papers.id (reviews + mq have
        //    on-delete-cascade FKs).
        sqlx::query("delete from papers where id = $1")
            .bind(paper_id)
            .execute(&pool)
            .await
            .ok();
    }
}
