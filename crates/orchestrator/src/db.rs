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
pub async fn insert_review(
    pool: &PgPool,
    paper_id: Uuid,
    models_used: Value,
    meta_review: Option<Value>,
) -> sqlx::Result<Uuid> {
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
    .execute(pool)
    .await?;
    Ok(id)
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
