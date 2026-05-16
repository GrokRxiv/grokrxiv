//! `GET /healthz`.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::state::AppState;

/// Liveness/readiness probe.
pub async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let db = match &state.db {
        Some(pool) => match sqlx::query("select 1").execute(pool).await {
            Ok(_) => "up",
            Err(_) => "down",
        },
        None => "absent",
    };
    Json(json!({
        "ok": db != "down",
        "version": env!("CARGO_PKG_VERSION"),
        "db": db,
    }))
}
