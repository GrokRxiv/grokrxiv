use std::time::Duration;

use super::{WorkItem, MAX_RETRIES};
use crate::state::AppState;
use grokrxiv_schemas::JobKind;
use tokio::sync::mpsc;
use uuid::Uuid;

const DEFAULT_SUPERVISOR_QUEUE_CAPACITY: usize = 4096;
const MIN_SUPERVISOR_QUEUE_CAPACITY: usize = 128;
const DEFAULT_SUPERVISOR_WORKER_LIMIT: usize = 64;
const MAX_SUPERVISOR_WORKER_LIMIT: usize = 1024;

pub(super) fn supervisor_queue_capacity() -> usize {
    supervisor_queue_capacity_from(
        std::env::var("GROKRXIV_SUPERVISOR_QUEUE_CAPACITY")
            .ok()
            .as_deref(),
    )
}

pub(super) fn supervisor_queue_capacity_from(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SUPERVISOR_QUEUE_CAPACITY)
        .max(MIN_SUPERVISOR_QUEUE_CAPACITY)
}

pub(super) fn supervisor_worker_limit() -> usize {
    supervisor_worker_limit_from(std::env::var("GROKRXIV_SUPERVISOR_WORKERS").ok().as_deref())
}

pub(super) fn supervisor_worker_limit_from(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SUPERVISOR_WORKER_LIMIT)
        .clamp(1, MAX_SUPERVISOR_WORKER_LIMIT)
}

pub(super) async fn run_item(
    state: &AppState,
    item: &WorkItem,
    supervisor_tx: &mpsc::Sender<WorkItem>,
) -> anyhow::Result<()> {
    if let Some(pool) = state.db.as_ref() {
        crate::db::mark_running(pool, item.job_id)
            .await
            .map_err(|e| anyhow::anyhow!("mark job running: {e}"))?;
    }
    let outcome = match item.kind {
        JobKind::Ingest => run_ingest(state, item, supervisor_tx).await,
        JobKind::Review => run_review(state, item).await,
        JobKind::Render => Err(anyhow::anyhow!("render: not implemented in M1")),
        JobKind::Publish => super::publish::run_publish(state, item).await,
        JobKind::Preview => Err(anyhow::anyhow!("preview is handled synchronously")),
    };
    match outcome {
        Ok(()) => {
            if let Some(pool) = state.db.as_ref() {
                crate::db::mark_done(pool, item.job_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("mark job done: {e}"))?;
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
                    let error = e.to_string();
                    if let Some(submission_id) = payload_uuid(item, "submission_id") {
                        let _ = crate::db::mark_submission_failed(pool, submission_id, &error)
                            .await
                            .map_err(|mark_err| {
                                tracing::warn!(
                                    %submission_id,
                                    err = %mark_err,
                                    "could not mark submission failed"
                                );
                                mark_err
                            });
                    }
                    crate::db::mark_failed(pool, item.job_id, &error)
                        .await
                        .map_err(|mark_err| {
                            anyhow::anyhow!("mark job failed: {mark_err}; original error: {error}")
                        })?;
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
    let submission_id = payload_uuid(item, "submission_id");
    if let Some(id) = submission_id {
        crate::db::mark_submission_running(pool, id, None).await?;
    }

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
    if let Some(id) = submission_id {
        crate::db::mark_submission_running(pool, id, Some(paper_id)).await?;
    }
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
                payload: item.payload.clone(),
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
    let submission_id = payload_uuid(item, "submission_id");
    let submitted_by = payload_uuid(item, "submitted_by");
    let visibility = payload_string(item, "visibility").unwrap_or_else(|| "public".to_string());
    let context = if submitted_by.is_some() || visibility != "public" {
        Some(super::review_flow::ReviewSubmissionContext {
            submitted_by,
            visibility: visibility.clone(),
        })
    } else {
        None
    };
    let review_id =
        super::review_flow::run_review_for_paper_full_with_context(state, paper_id, context)
            .await?;
    if let (Some(pool), Some(submission_id)) = (state.db.as_ref(), submission_id) {
        crate::db::mark_submission_review_ready(
            pool,
            submission_id,
            review_id,
            paper_id,
            &visibility,
        )
        .await?;
    }
    tracing::info!(%review_id, "review job complete — awaiting_moderation");
    Ok(())
}

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

pub(super) fn is_retryable(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("timeout") || s.contains("rate") || s.contains("temporar")
}

pub(super) fn exp_backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(std::cmp::min(base, 30_000))
}

fn payload_uuid(item: &WorkItem, key: &str) -> Option<Uuid> {
    item.payload
        .get(key)
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn payload_string(item: &WorkItem, key: &str) -> Option<String> {
    item.payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}
