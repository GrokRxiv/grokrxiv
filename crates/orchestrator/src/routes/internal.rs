//! `/internal/v1/*` — operator-facing write endpoints reached via the
//! Next.js `/api/v1/*` proxy.
//!
//! The proxy enforces a bearer token; these handlers run inside the
//! orchestrator's private network and do not re-auth.
//!
//! Write endpoints fail closed until they are backed by real supervisor
//! dispatch. Returning fake queued jobs here makes the web tier believe work
//! has started when nothing can run.
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
use serde::Deserialize;
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

/// Fail-closed handler for `POST /internal/v1/review`.
pub async fn review_post(Json(body): Json<ReviewRequest>) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!(source = %body.source, "internal/v1/review: dispatch not implemented");
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "internal review dispatch is not implemented",
            "status": "not_implemented",
            "source": body.source,
        })),
    )
}

fn action_not_implemented(id: Uuid, action: &'static str) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!(%id, action, "internal/v1 review action dispatch not implemented");
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "internal review action dispatch is not implemented",
            "status": "not_implemented",
            "review_id": id,
            "action": action,
        })),
    )
}

/// Fail-closed handler for `POST /internal/v1/reviews/:id/approve`.
pub async fn approve(Path(id): Path<Uuid>) -> (StatusCode, Json<serde_json::Value>) {
    action_not_implemented(id, "approve")
}

/// Fail-closed handler for `POST /internal/v1/reviews/:id/reject`.
pub async fn reject(Path(id): Path<Uuid>) -> (StatusCode, Json<serde_json::Value>) {
    action_not_implemented(id, "reject")
}

/// Fail-closed handler for `POST /internal/v1/reviews/:id/render`.
pub async fn render(Path(id): Path<Uuid>) -> (StatusCode, Json<serde_json::Value>) {
    action_not_implemented(id, "render")
}

/// Fail-closed handler for `POST /internal/v1/reviews/:id/apply-revisions`.
pub async fn apply_revisions(Path(id): Path<Uuid>) -> (StatusCode, Json<serde_json::Value>) {
    action_not_implemented(id, "apply_revisions")
}

/// Fail-closed handler for `POST /internal/v1/reviews/:id/verify`.
pub async fn verify(Path(id): Path<Uuid>) -> (StatusCode, Json<serde_json::Value>) {
    action_not_implemented(id, "verify")
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
