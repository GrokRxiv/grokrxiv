//! AgentHero platform orchestrator.
//!
//! This crate is intentionally app-neutral. Product code lives under
//! `agenthero/apps/<app>/` and is invoked through app manifests plus the
//! adapter protocol.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod config;
pub mod dag_apps;
pub mod doctor;
pub mod entrypoint;
pub mod serve;

/// Build the generic AgentHero router.
pub fn router() -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/apps",
            get(|| async {
                match dag_apps::registered_apps() {
                    Ok(apps) => axum::Json(serde_json::json!({ "apps": apps })).into_response(),
                    Err(err) => (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(serde_json::json!({ "error": err.to_string() })),
                    )
                        .into_response(),
                }
            }),
        )
}

use axum::response::IntoResponse;
