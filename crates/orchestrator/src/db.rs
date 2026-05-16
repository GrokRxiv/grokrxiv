//! Database helpers for the `jobs`, `reviews`, and `uploads` tables.
//!
//! All functions accept a borrowed [`sqlx::PgPool`] and use untyped queries
//! (`query_as`/`query`) so the crate builds even before migrations are
//! applied. Migration-driven `query!` macros can replace these later.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use grokrxiv_schemas::{AgentRole, JobKind, JobState, PaperExtract, ReviewStatus, VerifierStatus};

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
    let authors_json =
        serde_json::to_value(&extract.authors).unwrap_or_else(|_| Value::Array(vec![]));
    let id: Uuid = sqlx::query_scalar(
        "insert into papers (arxiv_id, title, authors, abstract, field, submitted_date)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (arxiv_id) do update set
           title = excluded.title,
           authors = excluded.authors,
           abstract = excluded.abstract,
           field = excluded.field,
           submitted_date = coalesce(excluded.submitted_date, papers.submitted_date)
         returning id",
    )
    .bind(&extract.arxiv_id)
    .bind(&extract.title)
    .bind(authors_json)
    .bind(extract.abstract_.as_str())
    .bind(extract.field.as_deref())
    .bind(submitted_date)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Insert a new review row for `paper_id` at `awaiting_moderation`. Returns
/// the new review id.
///
/// Any pre-existing review for the same paper in a non-terminal status is
/// transitioned to `withdrawn` first, so re-reviewing a paper SUPERSEDES the
/// old review rather than creating a parallel one. The previous reviews'
/// row ids (and `pr_url` from `moderation_queue`, if any) are returned so the
/// caller can close the old PR on the GitHub mirror.
pub async fn insert_review(
    pool: &PgPool,
    paper_id: Uuid,
    models_used: Value,
    meta_review: Option<Value>,
) -> sqlx::Result<Uuid> {
    let mut tx = pool.begin().await?;

    // Withdraw any active reviews for this paper. 'draft', 'in_review',
    // 'awaiting_moderation', 'pr_open', 'published', 'corrected' all count as
    // active and get superseded by the new run; 'withdrawn' rows are left as-is.
    sqlx::query(
        "update reviews \
         set status='withdrawn', superseded_at=now() \
         where paper_id=$1 \
           and status in ('draft','in_review','awaiting_moderation','pr_open','published','corrected')",
    )
    .bind(paper_id)
    .execute(&mut *tx)
    .await?;

    let id = Uuid::new_v4();
    let status = serde_plain(&ReviewStatus::AwaitingModeration);
    sqlx::query(
        "insert into reviews (id, paper_id, status, models_used, meta_review) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(paper_id)
    .bind(status)
    .bind(models_used)
    .bind(meta_review)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(id)
}

/// Look up the PR URL of the most recently superseded review for a paper, if
/// any. The publisher uses this to close the stale PR on `grokrxiv-reviews`
/// after the new review's PR is opened.
pub async fn fetch_superseded_pr_url(
    pool: &PgPool,
    paper_id: Uuid,
) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "select mq.pr_url \
         from reviews r \
         left join moderation_queue mq on mq.review_id = r.id \
         where r.paper_id = $1 \
           and r.status = 'withdrawn' \
           and r.superseded_at is not null \
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
    /// Agent role.
    pub role: AgentRole,
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
    let role_str = serde_plain(&row.role);
    let vstatus = row.verifier_status.as_ref().map(serde_plain);
    sqlx::query(
        "insert into review_agents \
           (id, review_id, role, model, output, verifier_status, \
            verifier_notes, tokens_in, tokens_out, latency_ms) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(row.review_id)
    .bind(role_str)
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
        "insert into review_inputs (review_id, paper_id, artifact) \
         values ($1, $2, $3) \
         on conflict (review_id) do update set artifact = excluded.artifact",
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
    role: AgentRole,
    content_hash: &str,
) -> sqlx::Result<Option<CachedOutput>> {
    let role_str = serde_plain(&role);
    let row: Option<(Value, String, String, Option<i32>, Option<i32>)> = sqlx::query_as(
        "select output, verifier_status, model, tokens_in, tokens_out \
         from review_cache \
         where paper_id = $1 and role = $2 and content_hash = $3 \
           and expires_at > now() \
         limit 1",
    )
    .bind(paper_id)
    .bind(role_str)
    .bind(content_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(output, verifier_status, model, tokens_in, tokens_out)| CachedOutput {
            output,
            verifier_status,
            model,
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
    role: AgentRole,
    content_hash: &str,
    output: &Value,
    verifier_status: &str,
    model: &str,
    tokens_in: Option<i32>,
    tokens_out: Option<i32>,
) -> sqlx::Result<()> {
    let role_str = serde_plain(&role);
    sqlx::query(
        "insert into review_cache \
           (paper_id, role, content_hash, output, verifier_status, model, tokens_in, tokens_out) \
         values ($1, $2, $3, $4, $5, $6, $7, $8) \
         on conflict (paper_id, role, content_hash) do update set \
           output = excluded.output, \
           verifier_status = excluded.verifier_status, \
           model = excluded.model, \
           tokens_in = excluded.tokens_in, \
           tokens_out = excluded.tokens_out, \
           created_at = now(), \
           expires_at = now() + interval '30 days'",
    )
    .bind(paper_id)
    .bind(role_str)
    .bind(content_hash)
    .bind(output)
    .bind(verifier_status)
    .bind(model)
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
/// Returns the new moderation row id. Called immediately after `insert_review`
/// so every review awaiting moderation has a matching queue entry.
pub async fn insert_moderation_pending(pool: &PgPool, review_id: Uuid) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into moderation_queue (id, review_id, state) \
         values ($1, $2, 'pending')",
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
