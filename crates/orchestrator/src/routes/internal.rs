//! `/internal/v1/*` — operator-facing write endpoints reached via the
//! Next.js `/api/v1/*` proxy.
//!
//! The proxy enforces a bearer token; these handlers run inside the
//! orchestrator's private network and do not re-auth.
//!
//! Review creation is backed by real supervisor dispatch. Other write actions
//! stay fail-closed until their moderation workers are wired.
//!
//! Routes:
//! - POST `/internal/v1/review`
//! - POST `/internal/v1/reviews/:id/approve`
//! - POST `/internal/v1/reviews/:id/reject`
//! - POST `/internal/v1/reviews/:id/render`
//! - POST `/internal/v1/reviews/:id/apply-revisions`
//! - POST `/internal/v1/reviews/:id/verify`
//! - GET  `/internal/v1/doctor`

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use grokrxiv_schemas::JobKind;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;
use crate::supervisor::WorkItem;

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
    /// Optional extractor override.
    #[serde(default)]
    pub extractor: Option<String>,
    /// Public/private review visibility.
    #[serde(default)]
    pub visibility: Option<String>,
    /// Billing/compute profile selected by the web tier.
    #[serde(default)]
    pub compute_profile: Option<String>,
    /// Consent for public review publication after moderation.
    #[serde(default)]
    pub public_consent: Option<bool>,
    /// Account user that requested the review.
    #[serde(default)]
    pub submitted_by: Option<Uuid>,
    /// User-facing submission row to update during the job lifecycle.
    #[serde(default)]
    pub submission_id: Option<Uuid>,
}

/// Enqueue `POST /internal/v1/review` into the live supervisor.
pub async fn review_post(
    State(state): State<AppState>,
    Json(body): Json<ReviewRequest>,
) -> Response {
    match enqueue_review(&state, body).await {
        Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
        Err((status, value)) => (status, Json(value)).into_response(),
    }
}

async fn enqueue_review(
    state: &AppState,
    body: ReviewRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let source = body.source.trim();
    if source.is_empty() {
        return Err(bad_request("source_required"));
    }
    let source_kind = body.r#type.as_deref().unwrap_or("arxiv");
    if source_kind != "arxiv" {
        return Err(bad_request("only_arxiv_sources_are_supported"));
    }
    let visibility = body.visibility.as_deref().unwrap_or("public");
    if !matches!(visibility, "public" | "private") {
        return Err(bad_request("bad_visibility"));
    }
    let compute_profile = body.compute_profile.as_deref().unwrap_or("public_free");
    if !matches!(
        compute_profile,
        "public_free" | "paid_standard" | "paid_private" | "premium_api"
    ) {
        return Err(bad_request("bad_compute_profile"));
    }
    if visibility == "public" && body.public_consent == Some(false) {
        return Err(bad_request("public_consent_required"));
    }

    let Some(pool) = state.db.as_ref() else {
        return Err(service_unavailable("database_not_configured"));
    };
    let Some(supervisor_tx) = state.supervisor_tx.as_ref() else {
        return Err(service_unavailable("supervisor_not_available"));
    };

    let job_id = crate::db::create_job(pool, JobKind::Ingest, None)
        .await
        .map_err(|e| internal_error("create_job_failed", e))?;
    let payload = review_payload(&body, source, visibility, compute_profile);
    supervisor_tx
        .send(WorkItem {
            job_id,
            kind: JobKind::Ingest,
            ref_id: None,
            payload,
            attempt: 0,
        })
        .await
        .map_err(|e| internal_error("supervisor_enqueue_failed", e))?;

    Ok(json!({
        "status": "queued",
        "job_id": job_id,
        "source": source,
        "source_type": source_kind,
        "submission_id": body.submission_id,
    }))
}

fn review_payload(
    body: &ReviewRequest,
    source: &str,
    visibility: &str,
    compute_profile: &str,
) -> serde_json::Value {
    json!({
        "arxiv_id": source,
        "auto_review": true,
        "visibility": visibility,
        "compute_profile": compute_profile,
        "runner": body.runner.as_deref(),
        "extractor": body.extractor.as_deref(),
        "submitted_by": body.submitted_by,
        "submission_id": body.submission_id,
    })
}

fn bad_request(code: &'static str) -> (StatusCode, serde_json::Value) {
    (StatusCode::BAD_REQUEST, json!({ "error": code }))
}

fn service_unavailable(code: &'static str) -> (StatusCode, serde_json::Value) {
    (StatusCode::SERVICE_UNAVAILABLE, json!({ "error": code }))
}

fn internal_error<E: std::fmt::Display>(
    code: &'static str,
    err: E,
) -> (StatusCode, serde_json::Value) {
    tracing::error!(error = %code, err = %err, "internal review dispatch failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": code, "detail": err.to_string() }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_payload_carries_account_context() {
        let submitted_by = Uuid::new_v4();
        let submission_id = Uuid::new_v4();
        let body = ReviewRequest {
            source: "2605.17307".to_string(),
            r#type: Some("arxiv".to_string()),
            mode: None,
            runner: Some("cli".to_string()),
            extractor: Some("cli".to_string()),
            visibility: Some("private".to_string()),
            compute_profile: Some("paid_private".to_string()),
            public_consent: Some(true),
            submitted_by: Some(submitted_by),
            submission_id: Some(submission_id),
        };

        let payload = review_payload(&body, "2605.17307", "private", "paid_private");

        assert_eq!(payload["arxiv_id"], "2605.17307");
        assert_eq!(payload["auto_review"], true);
        assert_eq!(payload["visibility"], "private");
        assert_eq!(payload["compute_profile"], "paid_private");
        assert_eq!(payload["submitted_by"], submitted_by.to_string());
        assert_eq!(payload["submission_id"], submission_id.to_string());
    }
}
