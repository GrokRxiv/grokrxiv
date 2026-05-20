//! Periodic arXiv ingest scheduler.
//!
//! On startup the scheduler optionally enqueues a backfill ingest covering
//! `backfill_from..today`; thereafter it fires once per UTC day at
//! `daily_at_utc` and enqueues a `from = yesterday, until = today` ingest.
//!
//! The actual fetch is performed by the sibling ingest crate. This module
//! handles cadence, range computation, and supervisor enqueueing.

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use grokrxiv_schemas::JobKind;

use crate::supervisor::WorkItem;

const BACKFILL_CHUNK_DAYS: u32 = 7;
const BACKFILL_CHUNK_PAUSE: Duration = Duration::from_millis(100);
const LISTING_MAX_ATTEMPTS: usize = 4;

/// Default arXiv categories to ingest: CS, Math, and Physics-cluster OAI sets.
///
/// Switching on the other five groups (q-bio, q-fin, stat, eess, econ) is a
/// one-line `INGEST_CATEGORIES` env change at deploy time.
pub const DEFAULT_ACTIVE_CATEGORIES: &[&str] = &[
    "cs", "math", "physics", "astro-ph", "cond-mat", "gr-qc", "hep-ex", "hep-lat", "hep-ph",
    "hep-th", "nucl-ex", "nucl-th", "quant-ph", "nlin",
];

/// Deprecated alias retained so older imports keep compiling. Prefer
/// [`DEFAULT_ACTIVE_CATEGORIES`].
#[deprecated(note = "use DEFAULT_ACTIVE_CATEGORIES")]
pub const DEFAULT_CATEGORIES: &[&str] = DEFAULT_ACTIVE_CATEGORIES;

/// Configuration for [`Scheduler::spawn`].
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// arXiv primary categories to poll, e.g. `["cs.AI", "cs.LG"]`.
    pub categories: Vec<String>,
    /// Earliest date to backfill on first start. Default 2025-01-01.
    pub backfill_from: NaiveDate,
    /// Papers submitted on or after this date will have a Review job
    /// auto-enqueued after ingest. Default 2026-04-01.
    pub auto_review_from: NaiveDate,
    /// UTC time of day to fire the daily ingest. Default 02:00.
    pub daily_at_utc: NaiveTime,
    /// User-Agent string to forward to the ingest worker.
    pub user_agent: String,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            categories: DEFAULT_ACTIVE_CATEGORIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            backfill_from: NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
            auto_review_from: NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
            daily_at_utc: NaiveTime::from_hms_opt(2, 0, 0).expect("valid time"),
            user_agent: "GrokRxiv/0.1 (mailto:mlong168@gmail.com)".to_string(),
        }
    }
}

impl SchedulerConfig {
    /// Build a config from an env-like `HashMap`, falling back to defaults.
    ///
    /// Recognized keys: `INGEST_CATEGORIES` (comma separated),
    /// `INGEST_BACKFILL_FROM` (YYYY-MM-DD), `AUTO_REVIEW_FROM` (YYYY-MM-DD),
    /// `INGEST_DAILY_AT_UTC` (HH:MM or HH:MM:SS), `ARXIV_USER_AGENT`.
    pub fn from_map(env: &HashMap<String, String>) -> Self {
        let d = SchedulerConfig::default();
        Self {
            categories: env
                .get("INGEST_CATEGORIES")
                .map(|s| {
                    s.split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect()
                })
                .unwrap_or(d.categories),
            backfill_from: env
                .get("INGEST_BACKFILL_FROM")
                .and_then(|s| NaiveDate::from_str(s).ok())
                .unwrap_or(d.backfill_from),
            auto_review_from: env
                .get("AUTO_REVIEW_FROM")
                .and_then(|s| NaiveDate::from_str(s).ok())
                .unwrap_or(d.auto_review_from),
            daily_at_utc: env
                .get("INGEST_DAILY_AT_UTC")
                .and_then(|s| parse_hhmm(s))
                .unwrap_or(d.daily_at_utc),
            user_agent: env.get("ARXIV_USER_AGENT").cloned().unwrap_or(d.user_agent),
        }
    }

    /// Build from process environment.
    pub fn from_env() -> Self {
        let env: HashMap<String, String> = [
            "INGEST_CATEGORIES",
            "INGEST_BACKFILL_FROM",
            "AUTO_REVIEW_FROM",
            "INGEST_DAILY_AT_UTC",
            "ARXIV_USER_AGENT",
        ]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
        Self::from_map(&env)
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M:%S") {
        return Some(t);
    }
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn chunk_range_by_days(
    from: NaiveDate,
    until: NaiveDate,
    chunk_days: u32,
) -> Vec<(NaiveDate, NaiveDate)> {
    if from > until {
        return Vec::new();
    }
    let chunk_days = chunk_days.max(1);
    let mut chunks = Vec::new();
    let mut start = from;
    loop {
        let end = start
            .checked_add_days(chrono::Days::new((chunk_days - 1).into()))
            .unwrap_or(until)
            .min(until);
        chunks.push((start, end));
        if end >= until {
            break;
        }
        let Some(next) = end.succ_opt() else {
            break;
        };
        start = next;
    }
    chunks
}

fn listing_retry_delay(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_secs(0),
        1 => Duration::from_secs(30),
        2 => Duration::from_secs(120),
        _ => Duration::from_secs(300),
    }
}

/// Payload for a ranged ingest job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRange {
    /// Inclusive start date (UTC).
    pub from: NaiveDate,
    /// Inclusive end date (UTC).
    pub until: NaiveDate,
    /// arXiv categories.
    pub categories: Vec<String>,
}

/// Scheduler handle. Only carries the supervisor sender; the long-running
/// loop is owned by the spawned task and accessed via [`Scheduler::spawn`].
pub struct Scheduler {
    /// Supervisor inbox for enqueueing ingest jobs.
    pub supervisor_tx: mpsc::Sender<WorkItem>,
}

impl Scheduler {
    /// Spawn the scheduler. Returns the [`JoinHandle`] of the background task.
    pub fn spawn(supervisor_tx: mpsc::Sender<WorkItem>, cfg: SchedulerConfig) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!(
                categories = ?cfg.categories,
                backfill_from = %cfg.backfill_from,
                auto_review_from = %cfg.auto_review_from,
                daily_at_utc = %cfg.daily_at_utc,
                "scheduler starting",
            );

            // Startup backfill can span many months. Run it in a sibling task
            // so boot can proceed to the daily loop immediately.
            let backfill_tx = supervisor_tx.clone();
            let backfill_cfg = cfg.clone();
            tokio::spawn(async move {
                run_startup_backfill(backfill_tx, backfill_cfg).await;
            });

            let mut retry_daily_from: Option<NaiveDate> = None;
            loop {
                let now = Utc::now();
                let next = next_fire_at(now, cfg.daily_at_utc);
                let sleep = (next - now)
                    .to_std()
                    .unwrap_or_else(|_| Duration::from_secs(60));
                tokio::time::sleep(sleep).await;

                let until = Utc::now().date_naive();
                let from = retry_daily_from.unwrap_or_else(|| until.pred_opt().unwrap_or(until));
                let daily = IngestRange {
                    from,
                    until,
                    categories: cfg.categories.clone(),
                };
                if enqueue_ingest_range_with_retries(&supervisor_tx, &cfg, &daily).await {
                    retry_daily_from = None;
                    tracing::info!(from = %from, until = %until, "daily ingest fired");
                } else {
                    retry_daily_from = Some(from);
                    tracing::warn!(
                        from = %from,
                        until = %until,
                        "daily ingest failed after retries; retaining range for next tick"
                    );
                }
            }
        })
    }
}

async fn run_startup_backfill(supervisor_tx: mpsc::Sender<WorkItem>, cfg: SchedulerConfig) {
    let today = Utc::now().date_naive();
    for (from, until) in chunk_range_by_days(cfg.backfill_from, today, BACKFILL_CHUNK_DAYS) {
        let chunk = IngestRange {
            from,
            until,
            categories: cfg.categories.clone(),
        };
        while !enqueue_ingest_range_with_retries(&supervisor_tx, &cfg, &chunk).await {
            tracing::warn!(
                from = %from,
                until = %until,
                "startup backfill chunk failed after retries; retrying same chunk"
            );
            tokio::time::sleep(listing_retry_delay(LISTING_MAX_ATTEMPTS)).await;
        }
        tracing::info!(from = %from, until = %until, "enqueued startup backfill chunk");
        tokio::time::sleep(BACKFILL_CHUNK_PAUSE).await;
    }
}

async fn enqueue_ingest_range_with_retries(
    supervisor_tx: &mpsc::Sender<WorkItem>,
    cfg: &SchedulerConfig,
    range: &IngestRange,
) -> bool {
    for attempt in 0..LISTING_MAX_ATTEMPTS {
        let delay = listing_retry_delay(attempt);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        match enqueue_ingest_range(supervisor_tx, cfg, range).await {
            Ok(count) => {
                tracing::info!(
                    from = %range.from,
                    until = %range.until,
                    count,
                    attempt = attempt + 1,
                    "scheduler listing range enqueued"
                );
                return true;
            }
            Err(e) => {
                tracing::warn!(
                    from = %range.from,
                    until = %range.until,
                    attempt = attempt + 1,
                    max_attempts = LISTING_MAX_ATTEMPTS,
                    err = %e,
                    "scheduler listing range failed"
                );
            }
        }
    }
    false
}

#[cfg(feature = "grokrxiv-ingest")]
async fn enqueue_ingest_range(
    supervisor_tx: &mpsc::Sender<WorkItem>,
    cfg: &SchedulerConfig,
    range: &IngestRange,
) -> anyhow::Result<usize> {
    let categories: Vec<&str> = range.categories.iter().map(String::as_str).collect();
    let records =
        grokrxiv_ingest::fetch_listing(&categories, range.from, range.until, &cfg.user_agent)
            .await?;
    let count = records.len();
    for meta in records {
        let item = ingest_work_item_from_listing_record(&meta, cfg.auto_review_from);
        supervisor_tx.send(item).await.map_err(|e| {
            anyhow::anyhow!("scheduler could not enqueue discovered ingest item: {e}")
        })?;
    }
    Ok(count)
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn enqueue_ingest_range(
    supervisor_tx: &mpsc::Sender<WorkItem>,
    _cfg: &SchedulerConfig,
    range: &IngestRange,
) -> anyhow::Result<usize> {
    supervisor_tx
        .send(WorkItem {
            job_id: Uuid::new_v4(),
            kind: JobKind::Ingest,
            ref_id: None,
            payload: serde_json::json!({
                "from": range.from.to_string(),
                "until": range.until.to_string(),
                "categories": range.categories,
            }),
            attempt: 0,
        })
        .await
        .map_err(|e| anyhow::anyhow!("scheduler could not enqueue fallback ingest range: {e}"))?;
    Ok(1)
}

/// Convert one OAI listing record into the single-paper work item the supervisor
/// ingest worker consumes.
#[cfg(feature = "grokrxiv-ingest")]
pub fn ingest_work_item_from_listing_record(
    meta: &grokrxiv_ingest::ArxivMeta,
    auto_review_from: NaiveDate,
) -> WorkItem {
    let auto_review = meta
        .submitted_date
        .map(|d| paper_in_auto_review_window(d, auto_review_from))
        .unwrap_or(false);
    WorkItem {
        job_id: Uuid::new_v4(),
        kind: JobKind::Ingest,
        ref_id: None,
        payload: serde_json::json!({
            "arxiv_id": meta.arxiv_id,
            "submitted_date": meta.submitted_date.map(|d| d.to_string()),
            "auto_review": auto_review,
        }),
        attempt: 0,
    }
}

/// Compute the next datetime at `time_of_day` UTC strictly in the future.
pub fn next_fire_at(now: DateTime<Utc>, time_of_day: NaiveTime) -> DateTime<Utc> {
    let today = now.date_naive();
    let candidate = today.and_time(time_of_day);
    let candidate_utc = Utc.from_utc_datetime(&candidate);
    if candidate_utc > now {
        candidate_utc
    } else {
        let tomorrow = today.succ_opt().unwrap_or_else(|| {
            today
                .checked_add_days(chrono::Days::new(1))
                .unwrap_or(today)
        });
        Utc.from_utc_datetime(&tomorrow.and_time(time_of_day))
    }
}

/// Predicate used by the ingest worker: should a freshly-ingested paper
/// auto-enqueue a Review job?
pub fn paper_in_auto_review_window(submitted_date: NaiveDate, auto_review_from: NaiveDate) -> bool {
    submitted_date >= auto_review_from
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn config_defaults_from_empty_map() {
        let cfg = SchedulerConfig::from_map(&HashMap::new());
        assert_eq!(
            cfg.backfill_from,
            NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()
        );
        assert_eq!(
            cfg.auto_review_from,
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()
        );
        assert_eq!(cfg.daily_at_utc, NaiveTime::from_hms_opt(2, 0, 0).unwrap());
        // Active set covers the three arXiv subject groupings: cs, math,
        // and the physics-cluster sets.
        assert!(cfg.categories.contains(&"cs".to_string()));
        assert!(cfg.categories.contains(&"math".to_string()));
        assert!(cfg.categories.contains(&"quant-ph".to_string()));
    }

    #[test]
    fn config_parses_overrides() {
        let env = [
            ("INGEST_CATEGORIES", "math.PR, stat.AP"),
            ("INGEST_BACKFILL_FROM", "2024-06-15"),
            ("AUTO_REVIEW_FROM", "2026-12-01"),
            ("INGEST_DAILY_AT_UTC", "14:30"),
            ("ARXIV_USER_AGENT", "X/1.0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let cfg = SchedulerConfig::from_map(&env);
        assert_eq!(cfg.categories, vec!["math.PR", "stat.AP"]);
        assert_eq!(
            cfg.backfill_from,
            NaiveDate::from_ymd_opt(2024, 6, 15).unwrap()
        );
        assert_eq!(
            cfg.auto_review_from,
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
        assert_eq!(
            cfg.daily_at_utc,
            NaiveTime::from_hms_opt(14, 30, 0).unwrap()
        );
        assert_eq!(cfg.user_agent, "X/1.0");
    }

    #[test]
    fn next_fire_today_when_before_target() {
        let now = ts(2026, 5, 13, 1, 0);
        let t = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let next = next_fire_at(now, t);
        assert_eq!(next, ts(2026, 5, 13, 2, 0));
    }

    #[test]
    fn next_fire_tomorrow_when_after_target() {
        let now = ts(2026, 5, 13, 23, 59);
        let t = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let next = next_fire_at(now, t);
        assert_eq!(next, ts(2026, 5, 14, 2, 0));
    }

    #[test]
    fn next_fire_tomorrow_when_exact_match() {
        let now = ts(2026, 5, 13, 2, 0);
        let t = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let next = next_fire_at(now, t);
        assert_eq!(next, ts(2026, 5, 14, 2, 0));
    }

    #[test]
    fn chunk_range_by_days_covers_inclusive_range_without_overlap() {
        let from = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();

        let chunks = chunk_range_by_days(from, until, 3);

        let expected = vec![
            (
                NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 3).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2026, 5, 4).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 6).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2026, 5, 7).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
            ),
        ];
        assert_eq!(chunks, expected);
    }

    #[test]
    fn listing_retry_delay_backs_off_and_caps() {
        assert_eq!(listing_retry_delay(0), Duration::from_secs(0));
        assert_eq!(listing_retry_delay(1), Duration::from_secs(30));
        assert_eq!(listing_retry_delay(2), Duration::from_secs(120));
        assert_eq!(listing_retry_delay(99), Duration::from_secs(300));
    }

    #[test]
    fn auto_review_predicate() {
        let cutoff = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        assert!(paper_in_auto_review_window(
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            cutoff
        ));
        assert!(paper_in_auto_review_window(cutoff, cutoff));
        assert!(!paper_in_auto_review_window(
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            cutoff
        ));
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn listing_record_builds_single_paper_ingest_work_item() {
        let meta = grokrxiv_ingest::ArxivMeta {
            arxiv_id: "2605.00001".to_string(),
            title: "Queued paper".to_string(),
            authors: Vec::new(),
            abstract_text: "abstract".to_string(),
            categories: vec!["cs.AI".to_string()],
            pdf_url: None,
            source_url: None,
            submitted_date: Some(NaiveDate::from_ymd_opt(2026, 5, 13).unwrap()),
        };
        let cutoff = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();

        let item = ingest_work_item_from_listing_record(&meta, cutoff);

        assert_eq!(item.kind, JobKind::Ingest);
        assert_eq!(
            item.payload.get("arxiv_id").and_then(|v| v.as_str()),
            Some("2605.00001")
        );
        assert_eq!(
            item.payload.get("submitted_date").and_then(|v| v.as_str()),
            Some("2026-05-13")
        );
        assert_eq!(
            item.payload.get("auto_review").and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
