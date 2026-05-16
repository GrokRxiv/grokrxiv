//! `/internal/v1/*` — operator-facing write endpoints reached via the
//! Next.js `/api/v1/*` proxy.
//!
//! The proxy enforces a bearer token; these handlers run inside the
//! orchestrator's private network and do not re-auth.
//!
//! For RPT2 Track I the handlers return a structured stub payload so the
//! proxy + the CLI flows have a stable shape to test against; the full
//! async-job enqueue work is a Track I follow-up.
//!
//! Routes:
//! - POST `/internal/v1/review`
//! - POST `/internal/v1/reviews/:id/approve`
//! - POST `/internal/v1/reviews/:id/reject`
//! - POST `/internal/v1/reviews/:id/render`
//! - POST `/internal/v1/reviews/:id/apply-revisions`
//! - POST `/internal/v1/reviews/:id/verify`
//! - GET  `/internal/v1/doctor`

use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request body for `POST /internal/v1/review`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ReviewRequest {
    /// Source identifier: arXiv id, URL, local path, `-`, or `@file`.
    pub source: String,
    /// Optional explicit source kind.
    #[serde(default)]
    pub r#type: Option<String>,
    /// Optional review mode.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional default runner override.
    #[serde(default)]
    pub runner: Option<String>,
}

/// Response payload for `POST /internal/v1/review`.
#[derive(Debug, Serialize)]
pub struct EnqueuedJob {
    /// Synthetic job id (will become a real `jobs.id` once the async dispatch lands).
    pub job_id: String,
    /// Coarse-grained status.
    pub status: &'static str,
    /// Operator-facing note explaining the stub.
    pub note: &'static str,
}

/// Stub handler for `POST /internal/v1/review`.
pub async fn review_post(
    Json(body): Json<ReviewRequest>,
) -> (StatusCode, Json<EnqueuedJob>) {
    tracing::info!(source = %body.source, "internal/v1/review: stub enqueue");
    (
        StatusCode::ACCEPTED,
        Json(EnqueuedJob {
            job_id: Uuid::new_v4().to_string(),
            status: "queued",
            note: "stub enqueue (Track I follow-up will wire async dispatch to supervisor)",
        }),
    )
}

/// Generic acknowledgement payload returned by every per-review action stub.
#[derive(Debug, Serialize)]
pub struct ActionAck {
    /// Review id the action targeted.
    pub review_id: Uuid,
    /// Action name (`approve` | `reject` | `render` | `apply_revisions` | `verify`).
    pub action: &'static str,
    /// Coarse-grained status.
    pub status: &'static str,
    /// Operator-facing note explaining the stub.
    pub note: &'static str,
}

/// Stub handler for `POST /internal/v1/reviews/:id/approve`.
pub async fn approve(Path(id): Path<Uuid>) -> (StatusCode, Json<ActionAck>) {
    (
        StatusCode::ACCEPTED,
        Json(ActionAck {
            review_id: id,
            action: "approve",
            status: "queued",
            note: "stub — use `grokrxiv approve <id>` for the synchronous path",
        }),
    )
}

/// Stub handler for `POST /internal/v1/reviews/:id/reject`.
pub async fn reject(Path(id): Path<Uuid>) -> (StatusCode, Json<ActionAck>) {
    (
        StatusCode::ACCEPTED,
        Json(ActionAck {
            review_id: id,
            action: "reject",
            status: "queued",
            note: "stub — use `grokrxiv reject <id> --reason TEXT` for the synchronous path",
        }),
    )
}

/// Stub handler for `POST /internal/v1/reviews/:id/render`.
pub async fn render(Path(id): Path<Uuid>) -> (StatusCode, Json<ActionAck>) {
    (
        StatusCode::ACCEPTED,
        Json(ActionAck {
            review_id: id,
            action: "render",
            status: "queued",
            note: "stub — use `grokrxiv render <id> --format html` for the synchronous path",
        }),
    )
}

/// Stub handler for `POST /internal/v1/reviews/:id/apply-revisions`.
pub async fn apply_revisions(Path(id): Path<Uuid>) -> (StatusCode, Json<ActionAck>) {
    (
        StatusCode::ACCEPTED,
        Json(ActionAck {
            review_id: id,
            action: "apply_revisions",
            status: "queued",
            note: "stub — revision application is a Track F follow-up",
        }),
    )
}

/// Stub handler for `POST /internal/v1/reviews/:id/verify`.
pub async fn verify(Path(id): Path<Uuid>) -> (StatusCode, Json<ActionAck>) {
    (
        StatusCode::ACCEPTED,
        Json(ActionAck {
            review_id: id,
            action: "verify",
            status: "queued",
            note: "stub — use `grokrxiv verify <id>` for the synchronous path",
        }),
    )
}

/// Lightweight `/internal/v1/doctor` summary for web-tier polling.
///
/// Returns the same shape as the CLI's `--json` but only the boolean
/// presence/absence of each provider key — no outbound HTTPS pings, so this
/// is safe to call on every health check. Operators can shell out to the
/// CLI's `grokrxiv doctor --json` for the full structured report.
pub async fn doctor() -> Json<serde_json::Value> {
    let database_url = std::env::var("DATABASE_URL").is_ok();
    let anthropic = std::env::var("ANTHROPIC_API_KEY").is_ok();
    let openai = std::env::var("OPENAI_API_KEY").is_ok();
    let gemini = std::env::var("GOOGLE_GENERATIVE_AI_API_KEY").is_ok();
    Json(serde_json::json!({
        "profile": "default",
        "database_url": database_url,
        "api_runners": {
            "anthropic": anthropic,
            "openai": openai,
            "gemini": gemini,
        },
        "note": "lightweight summary; use `grokrxiv doctor --json` for the full structured report",
    }))
}
