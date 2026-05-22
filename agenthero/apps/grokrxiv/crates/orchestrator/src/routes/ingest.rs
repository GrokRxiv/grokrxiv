//! `POST /ingest` — admin-only enqueue of an arXiv id.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use grokrxiv_schemas::JobKind;
use serde::Deserialize;
use serde_json::json;

use crate::state::AppState;

/// Request body for `POST /ingest`.
#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// arXiv id (e.g. `2401.12345` or `2401.12345v2`).
    pub arxiv_id: String,
}

/// Enqueue an ingest job for the given arXiv id. Returns `{ job_id }`.
pub async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IngestRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_admin(&state, &headers) {
        return resp;
    }
    if body.arxiv_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "arxiv_id required" })),
        )
            .into_response();
    }

    let Some(pool) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        )
            .into_response();
    };
    let job_id = match crate::db::create_job(pool, JobKind::Ingest, None).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(err = %e, "create_job failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "db insert failed" })),
            )
                .into_response();
        }
    };

    (
        StatusCode::ACCEPTED,
        Json(json!({ "job_id": job_id, "arxiv_id": body.arxiv_id })),
    )
        .into_response()
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), axum::response::Response> {
    let Some(expected) = state.config.admin_token.as_deref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "ADMIN_TOKEN not configured" })),
        )
            .into_response());
    };
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if provided != Some(expected) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid admin token" })),
        )
            .into_response());
    }
    Ok(())
}
