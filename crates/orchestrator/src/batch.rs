use std::collections::BTreeMap;

use anyhow::Context as _;
use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct BatchCreateOptions {
    pub(crate) category: String,
    pub(crate) from: NaiveDate,
    pub(crate) until: NaiveDate,
    pub(crate) daily_limit: usize,
    pub(crate) auto_pr: bool,
    pub(crate) start_date: NaiveDate,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatchCreateResult {
    pub(crate) batch_id: Option<Uuid>,
    pub(crate) category: String,
    pub(crate) from: NaiveDate,
    pub(crate) until: NaiveDate,
    pub(crate) daily_limit: usize,
    pub(crate) auto_pr: bool,
    pub(crate) discovered: usize,
    pub(crate) scheduled_days: usize,
    pub(crate) first_items: Vec<BatchItemPreview>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatchItemPreview {
    pub(crate) arxiv_id: String,
    pub(crate) title: String,
    pub(crate) primary_category: Option<String>,
    pub(crate) submitted_date: Option<NaiveDate>,
    pub(crate) position: usize,
    pub(crate) scheduled_for: NaiveDate,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatchRow {
    pub(crate) id: Uuid,
    pub(crate) category: String,
    pub(crate) from: NaiveDate,
    pub(crate) until: NaiveDate,
    pub(crate) daily_limit: usize,
    pub(crate) auto_pr: bool,
    pub(crate) state: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatchItemRow {
    pub(crate) id: Uuid,
    pub(crate) batch_id: Uuid,
    pub(crate) arxiv_id: String,
    pub(crate) title: String,
    pub(crate) primary_category: Option<String>,
    pub(crate) submitted_date: Option<NaiveDate>,
    pub(crate) position: usize,
    pub(crate) scheduled_for: NaiveDate,
    pub(crate) state: String,
    pub(crate) paper_id: Option<Uuid>,
    pub(crate) review_id: Option<Uuid>,
    pub(crate) job_id: Option<Uuid>,
    pub(crate) pr_url: Option<String>,
    pub(crate) attempts: i32,
    pub(crate) error: Option<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatchStatus {
    pub(crate) batch: BatchRow,
    pub(crate) counts: BTreeMap<String, i64>,
    pub(crate) next_items: Vec<BatchItemRow>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobListRow {
    pub(crate) id: Uuid,
    pub(crate) kind: String,
    pub(crate) ref_id: Option<Uuid>,
    pub(crate) state: String,
    pub(crate) attempt: i32,
    pub(crate) error: Option<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) finished_at: Option<DateTime<Utc>>,
    pub(crate) created_at: DateTime<Utc>,
}

#[cfg(feature = "grokrxiv-ingest")]
pub(crate) fn preview_batch(
    options: &BatchCreateOptions,
    records: &[grokrxiv_ingest::ArxivMeta],
) -> BatchCreateResult {
    let ordered = ordered_records(options, records);
    let first_items = ordered
        .iter()
        .take(12)
        .enumerate()
        .map(|(idx, meta)| preview_item(options, meta, idx))
        .collect();
    BatchCreateResult {
        batch_id: None,
        category: options.category.clone(),
        from: options.from,
        until: options.until,
        daily_limit: options.daily_limit,
        auto_pr: options.auto_pr,
        discovered: ordered.len(),
        scheduled_days: scheduled_days(ordered.len(), options.daily_limit),
        first_items,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(crate) async fn create_batch(
    pool: &PgPool,
    options: &BatchCreateOptions,
    records: &[grokrxiv_ingest::ArxivMeta],
) -> anyhow::Result<BatchCreateResult> {
    let ordered = ordered_records(options, records);
    let daily_limit = i32::try_from(options.daily_limit)
        .context("daily_limit is too large for database storage")?;
    let batch_id: Uuid = sqlx::query_scalar(
        "insert into review_batches (category, from_date, until_date, daily_limit, auto_pr) \
         values ($1, $2, $3, $4, $5) \
         returning id",
    )
    .bind(&options.category)
    .bind(options.from)
    .bind(options.until)
    .bind(daily_limit)
    .bind(options.auto_pr)
    .fetch_one(pool)
    .await?;

    for (idx, meta) in ordered.iter().enumerate() {
        let position = i32::try_from(idx).context("batch item position is too large")?;
        let scheduled_for = schedule_for_position(options.start_date, idx, options.daily_limit);
        sqlx::query(
            "insert into review_batch_items \
             (batch_id, arxiv_id, title, primary_category, submitted_date, position, scheduled_for) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             on conflict (batch_id, arxiv_id) do nothing",
        )
        .bind(batch_id)
        .bind(&meta.arxiv_id)
        .bind(&meta.title)
        .bind(meta.primary_category())
        .bind(meta.submitted_date)
        .bind(position)
        .bind(scheduled_for)
        .execute(pool)
        .await?;
    }

    let mut result = preview_batch(options, records);
    result.batch_id = Some(batch_id);
    Ok(result)
}

pub(crate) fn parse_month_range(month: &str) -> anyhow::Result<(NaiveDate, NaiveDate)> {
    let (year, month_num) = month
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("month must be YYYY-MM"))?;
    let year: i32 = year.parse().context("month year must be numeric")?;
    let month_num: u32 = month_num.parse().context("month must be numeric")?;
    let first = NaiveDate::from_ymd_opt(year, month_num, 1)
        .ok_or_else(|| anyhow::anyhow!("invalid month `{month}`"))?;
    let (next_year, next_month) = if month_num == 12 {
        (year + 1, 1)
    } else {
        (year, month_num + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| anyhow::anyhow!("invalid month `{month}`"))?;
    let last = next_first
        .pred_opt()
        .ok_or_else(|| anyhow::anyhow!("could not compute last day for `{month}`"))?;
    Ok((first, last))
}

pub(crate) fn schedule_for_position(
    start_date: NaiveDate,
    position: usize,
    daily_limit: usize,
) -> NaiveDate {
    let day_offset = position / daily_limit.max(1);
    start_date
        .checked_add_days(Days::new(day_offset as u64))
        .unwrap_or(start_date)
}

pub(crate) async fn load_batch(pool: &PgPool, batch_id: Uuid) -> anyhow::Result<BatchRow> {
    let row: (
        Uuid,
        String,
        NaiveDate,
        NaiveDate,
        i32,
        bool,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
    ) = sqlx::query_as(
        "select id, category, from_date, until_date, daily_limit, auto_pr, state, created_at, updated_at \
         from review_batches \
         where id = $1",
    )
    .bind(batch_id)
    .fetch_one(pool)
    .await?;
    Ok(batch_row_from_tuple(row))
}

pub(crate) async fn list_batches(pool: &PgPool, limit: i64) -> anyhow::Result<Vec<BatchStatus>> {
    let rows: Vec<(
        Uuid,
        String,
        NaiveDate,
        NaiveDate,
        i32,
        bool,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "select id, category, from_date, until_date, daily_limit, auto_pr, state, created_at, updated_at \
         from review_batches \
         order by created_at desc \
         limit $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let batch = batch_row_from_tuple(row);
        out.push(BatchStatus {
            counts: load_batch_counts(pool, batch.id).await?,
            next_items: load_next_items(pool, batch.id, 3).await?,
            batch,
        });
    }
    Ok(out)
}

pub(crate) async fn load_batch_status(
    pool: &PgPool,
    batch_id: Uuid,
) -> anyhow::Result<BatchStatus> {
    let batch = load_batch(pool, batch_id).await?;
    let counts = load_batch_counts(pool, batch_id).await?;
    let next_items = load_next_items(pool, batch_id, 12).await?;
    Ok(BatchStatus {
        batch,
        counts,
        next_items,
    })
}

pub(crate) async fn due_batch_items(
    pool: &PgPool,
    batch_id: Uuid,
    today: NaiveDate,
    limit: i64,
) -> anyhow::Result<Vec<BatchItemRow>> {
    let rows: Vec<BatchItemDbRow> = sqlx::query_as(
        "select id, batch_id, arxiv_id, title, primary_category, submitted_date, position, scheduled_for, \
                state, paper_id, review_id, job_id, pr_url, attempts, error, started_at, finished_at \
         from review_batch_items \
         where batch_id = $1 and state = 'queued' and scheduled_for <= $2 \
         order by scheduled_for asc, position asc \
         limit $3",
    )
    .bind(batch_id)
    .bind(today)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(item_rows_from_db(rows))
}

pub(crate) async fn claim_due_batch_items(
    pool: &PgPool,
    batch_id: Uuid,
    today: NaiveDate,
    limit: i64,
) -> anyhow::Result<Vec<BatchItemRow>> {
    let mut tx = pool.begin().await?;
    let rows: Vec<BatchItemDbRow> = sqlx::query_as(
        "select id, batch_id, arxiv_id, title, primary_category, submitted_date, position, scheduled_for, \
                state, paper_id, review_id, job_id, pr_url, attempts, error, started_at, finished_at \
         from review_batch_items \
         where batch_id = $1 and state = 'queued' and scheduled_for <= $2 \
         order by scheduled_for asc, position asc \
         limit $3 \
         for update skip locked",
    )
    .bind(batch_id)
    .bind(today)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    if !ids.is_empty() {
        sqlx::query(
            "update review_batch_items \
             set state = 'running', attempts = attempts + 1, started_at = now(), updated_at = now(), error = null \
             where id = any($1)",
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let mut items = item_rows_from_db(rows);
    for item in &mut items {
        item.state = "running".to_string();
        item.attempts += 1;
        item.error = None;
    }
    Ok(items)
}

pub(crate) async fn mark_item_succeeded(
    pool: &PgPool,
    item_id: Uuid,
    paper_id: Option<Uuid>,
    review_id: Uuid,
    job_id: Option<Uuid>,
    pr_url: Option<&str>,
) -> anyhow::Result<()> {
    let state = if pr_url.is_some() {
        "pr_open"
    } else {
        "reviewed"
    };
    sqlx::query(
        "update review_batch_items \
         set state = $2, paper_id = $3, review_id = $4, job_id = $5, pr_url = $6, \
             error = null, finished_at = now(), updated_at = now() \
         where id = $1",
    )
    .bind(item_id)
    .bind(state)
    .bind(paper_id)
    .bind(review_id)
    .bind(job_id)
    .bind(pr_url)
    .execute(pool)
    .await?;
    refresh_batch_state_for_item(pool, item_id).await
}

pub(crate) async fn mark_item_failed(
    pool: &PgPool,
    item_id: Uuid,
    paper_id: Option<Uuid>,
    review_id: Option<Uuid>,
    job_id: Option<Uuid>,
    error: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "update review_batch_items \
         set state = 'failed', paper_id = $2, review_id = $3, job_id = $4, error = $5, \
             finished_at = now(), updated_at = now() \
         where id = $1",
    )
    .bind(item_id)
    .bind(paper_id)
    .bind(review_id)
    .bind(job_id)
    .bind(error)
    .execute(pool)
    .await?;
    refresh_batch_state_for_item(pool, item_id).await
}

pub(crate) async fn latest_review_job_for_paper(
    pool: &PgPool,
    paper_id: Uuid,
) -> anyhow::Result<Option<Uuid>> {
    let job_id = sqlx::query_scalar(
        "select id from jobs where kind = 'review' and ref_id = $1 order by created_at desc limit 1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(job_id)
}

pub(crate) async fn list_jobs(
    pool: &PgPool,
    kind: Option<&str>,
    state: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<JobListRow>> {
    let rows: Vec<(
        Uuid,
        String,
        Option<Uuid>,
        String,
        i32,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "select id, kind, ref_id, state, attempt, error, started_at, finished_at, created_at \
         from jobs \
         where ($1::text is null or kind = $1) \
           and ($2::text is null or state = $2) \
         order by created_at desc \
         limit $3",
    )
    .bind(kind)
    .bind(state)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, kind, ref_id, state, attempt, error, started_at, finished_at, created_at)| {
                JobListRow {
                    id,
                    kind,
                    ref_id,
                    state,
                    attempt,
                    error,
                    started_at,
                    finished_at,
                    created_at,
                }
            },
        )
        .collect())
}

#[cfg(feature = "grokrxiv-ingest")]
fn ordered_records(
    options: &BatchCreateOptions,
    records: &[grokrxiv_ingest::ArxivMeta],
) -> Vec<grokrxiv_ingest::ArxivMeta> {
    let mut ordered = records.to_vec();
    ordered.retain(|meta| {
        meta.submitted_date
            .map(|date| date >= options.from && date <= options.until)
            .unwrap_or(false)
    });
    ordered.sort_by(|a, b| {
        a.submitted_date
            .cmp(&b.submitted_date)
            .then_with(|| a.arxiv_id.cmp(&b.arxiv_id))
    });
    ordered
}

#[cfg(feature = "grokrxiv-ingest")]
fn preview_item(
    options: &BatchCreateOptions,
    meta: &grokrxiv_ingest::ArxivMeta,
    idx: usize,
) -> BatchItemPreview {
    BatchItemPreview {
        arxiv_id: meta.arxiv_id.clone(),
        title: meta.title.clone(),
        primary_category: meta.primary_category(),
        submitted_date: meta.submitted_date,
        position: idx,
        scheduled_for: schedule_for_position(options.start_date, idx, options.daily_limit),
    }
}

fn scheduled_days(total: usize, daily_limit: usize) -> usize {
    if total == 0 {
        0
    } else {
        total.div_ceil(daily_limit.max(1))
    }
}

fn batch_row_from_tuple(
    row: (
        Uuid,
        String,
        NaiveDate,
        NaiveDate,
        i32,
        bool,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
    ),
) -> BatchRow {
    BatchRow {
        id: row.0,
        category: row.1,
        from: row.2,
        until: row.3,
        daily_limit: row.4.max(1) as usize,
        auto_pr: row.5,
        state: row.6,
        created_at: row.7,
        updated_at: row.8,
    }
}

async fn load_batch_counts(pool: &PgPool, batch_id: Uuid) -> anyhow::Result<BTreeMap<String, i64>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "select state, count(*)::bigint from review_batch_items where batch_id = $1 group by state",
    )
    .bind(batch_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

async fn load_next_items(
    pool: &PgPool,
    batch_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<BatchItemRow>> {
    let rows: Vec<BatchItemDbRow> = sqlx::query_as(
        "select id, batch_id, arxiv_id, title, primary_category, submitted_date, position, scheduled_for, \
                state, paper_id, review_id, job_id, pr_url, attempts, error, started_at, finished_at \
         from review_batch_items \
         where batch_id = $1 and state in ('queued', 'running', 'failed') \
         order by scheduled_for asc, position asc \
         limit $2",
    )
    .bind(batch_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(item_rows_from_db(rows))
}

async fn refresh_batch_state_for_item(pool: &PgPool, item_id: Uuid) -> anyhow::Result<()> {
    let Some(batch_id): Option<Uuid> =
        sqlx::query_scalar("select batch_id from review_batch_items where id = $1")
            .bind(item_id)
            .fetch_optional(pool)
            .await?
    else {
        return Ok(());
    };
    let pending: i64 = sqlx::query_scalar(
        "select count(*)::bigint from review_batch_items \
         where batch_id = $1 and state in ('queued', 'running')",
    )
    .bind(batch_id)
    .fetch_one(pool)
    .await?;
    if pending > 0 {
        return Ok(());
    }
    let failed: i64 = sqlx::query_scalar(
        "select count(*)::bigint from review_batch_items where batch_id = $1 and state = 'failed'",
    )
    .bind(batch_id)
    .fetch_one(pool)
    .await?;
    let state = if failed > 0 { "failed" } else { "done" };
    sqlx::query("update review_batches set state = $2, updated_at = now() where id = $1")
        .bind(batch_id)
        .bind(state)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct BatchItemDbRow {
    id: Uuid,
    batch_id: Uuid,
    arxiv_id: String,
    title: String,
    primary_category: Option<String>,
    submitted_date: Option<NaiveDate>,
    position: i32,
    scheduled_for: NaiveDate,
    state: String,
    paper_id: Option<Uuid>,
    review_id: Option<Uuid>,
    job_id: Option<Uuid>,
    pr_url: Option<String>,
    attempts: i32,
    error: Option<String>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

fn item_rows_from_db(rows: Vec<BatchItemDbRow>) -> Vec<BatchItemRow> {
    rows.into_iter()
        .map(|row| BatchItemRow {
            id: row.id,
            batch_id: row.batch_id,
            arxiv_id: row.arxiv_id,
            title: row.title,
            primary_category: row.primary_category,
            submitted_date: row.submitted_date,
            position: row.position.max(0) as usize,
            scheduled_for: row.scheduled_for,
            state: row.state,
            paper_id: row.paper_id,
            review_id: row.review_id,
            job_id: row.job_id,
            pr_url: row.pr_url,
            attempts: row.attempts,
            error: row.error,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_range_handles_regular_and_leap_months() {
        assert_eq!(
            parse_month_range("2026-05").unwrap(),
            (
                NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 31).unwrap()
            )
        );
        assert_eq!(
            parse_month_range("2024-02").unwrap().1,
            NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()
        );
    }

    #[test]
    fn schedule_respects_daily_limit() {
        let start = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        assert_eq!(schedule_for_position(start, 0, 30), start);
        assert_eq!(schedule_for_position(start, 29, 30), start);
        assert_eq!(
            schedule_for_position(start, 30, 30),
            NaiveDate::from_ymd_opt(2026, 5, 2).unwrap()
        );
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn preview_filters_oai_updates_to_month_submissions() {
        let options = BatchCreateOptions {
            category: "math".to_string(),
            from: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 5, 31).unwrap(),
            daily_limit: 30,
            auto_pr: true,
            start_date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        };
        let records = vec![
            grokrxiv_ingest::ArxivMeta {
                arxiv_id: "math/0209080".to_string(),
                title: "Old metadata update".to_string(),
                submitted_date: Some(NaiveDate::from_ymd_opt(2002, 9, 10).unwrap()),
                ..Default::default()
            },
            grokrxiv_ingest::ArxivMeta {
                arxiv_id: "2605.00001".to_string(),
                title: "New May paper".to_string(),
                categories: vec!["math.AG".to_string()],
                submitted_date: Some(NaiveDate::from_ymd_opt(2026, 5, 2).unwrap()),
                ..Default::default()
            },
        ];

        let result = preview_batch(&options, &records);

        assert_eq!(result.discovered, 1);
        assert_eq!(result.first_items[0].arxiv_id, "2605.00001");
    }
}
