//! Runtime configuration loaded from environment variables.

use std::env;

use crate::scheduler::SchedulerConfig;

/// Configuration loaded once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    /// Bind address for the HTTP server, e.g. `0.0.0.0:8080`.
    pub bind: String,
    /// Optional Supabase Postgres URL. When `None` the orchestrator runs in
    /// "stateless" mode (no DB writes); useful for local M1 demos before
    /// migrations land.
    pub database_url: Option<String>,
    /// Required bearer token for admin endpoints (`/ingest`).
    pub admin_token: Option<String>,
    /// HMAC secret used to verify GitHub webhook signatures.
    pub github_webhook_secret: Option<String>,
    /// Frontend revalidation URL hit on publish.
    pub web_revalidate_url: Option<String>,
    /// Shared secret sent to the revalidate endpoint.
    pub revalidate_secret: Option<String>,
    /// User-Agent string used when talking to arXiv. Defaults to a stable
    /// string that includes the project contact.
    pub arxiv_user_agent: String,
    /// Anthropic model name used by the single-pass `/preview` path. Overridable
    /// via `PREVIEW_MODEL`; defaults to `claude-opus-4-7`.
    pub preview_model: String,
    /// Scheduler tuning (categories, backfill window, auto-review cutoff). The
    /// scheduler task uses this directly; the supervisor reads
    /// `scheduler.auto_review_from` when deciding whether a freshly-ingested
    /// paper auto-enqueues a Review job.
    pub scheduler: SchedulerConfig,
}

impl Config {
    /// Construct from process environment.
    pub fn from_env() -> Self {
        Self {
            bind: env::var("ORCHESTRATOR_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("DATABASE_URL").ok(),
            admin_token: env::var("ADMIN_TOKEN").ok(),
            github_webhook_secret: env::var("GITHUB_WEBHOOK_SECRET").ok(),
            web_revalidate_url: env::var("WEB_REVALIDATE_URL").ok(),
            revalidate_secret: env::var("REVALIDATE_SECRET").ok(),
            arxiv_user_agent: env::var("ARXIV_USER_AGENT")
                .unwrap_or_else(|_| "GrokRxiv/0.1 (mailto:mlong168@gmail.com)".into()),
            preview_model: env::var("PREVIEW_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into()),
            scheduler: SchedulerConfig::from_env(),
        }
    }
}
