//! AgentHero platform orchestrator.
//!
//! This crate is intentionally app-neutral. Product code lives under
//! `agenthero/apps/<app>/` and is invoked through app manifests plus the
//! adapter protocol.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app_runs;
pub mod cli;
pub mod config;
pub mod dag_apps;
pub mod doctor;
pub mod entrypoint;
pub mod scheduler;
pub mod serve;

/// Build the generic AgentHero router.
pub fn router() -> axum::Router {
    router_with_state(PlatformState::default())
}

/// Shared state for the generic AgentHero HTTP API.
#[derive(Clone, Default)]
pub struct PlatformState {
    /// Optional platform database pool.
    pub pool: Option<sqlx::PgPool>,
    /// Optional bearer token for private write routes.
    pub service_token: Option<String>,
}

/// Build the generic AgentHero router with explicit state.
pub fn router_with_state(state: PlatformState) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/apps", get(apps_index))
        .route("/apps/:app", get(apps_show))
        .route("/apps/:app/actions/:action/runs", post(enqueue_app_run))
        .route("/app-runs", get(app_runs_index))
        .route("/app-runs/:id", get(app_runs_show))
        .route("/app-runs/:id/events", get(app_run_events))
        .with_state(state)
}

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

async fn apps_index() -> impl IntoResponse {
    match dag_apps::registered_apps() {
        Ok(apps) => Json(json!({ "apps": apps })).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "apps_load_failed", err),
    }
}

async fn apps_show(AxumPath(app): AxumPath<String>) -> impl IntoResponse {
    match dag_apps::registered_app(&app) {
        Ok(Some(app)) => Json(app).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "app_not_found", "app not found"),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "apps_load_failed", err),
    }
}

async fn enqueue_app_run(
    State(state): State<PlatformState>,
    headers: HeaderMap,
    AxumPath((app, action)): AxumPath<(String, String)>,
    Json(body): Json<app_runs::AppRunRequest>,
) -> impl IntoResponse {
    if let Err(response) = authorize_write(&state, &headers) {
        return response;
    }
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    if let Err(err) = dag_apps::app_action_binding(&app, &action) {
        return error_response(StatusCode::NOT_FOUND, "app_action_not_found", err);
    }
    match app_runs::insert_queued(pool, &app, &action, body).await {
        Ok(run_id) => (
            StatusCode::ACCEPTED,
            Json(json!({ "run_id": run_id, "state": "queued" })),
        )
            .into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "enqueue_failed", err),
    }
}

#[derive(Debug, Deserialize)]
struct AppRunsQuery {
    app: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
}

async fn app_runs_index(
    State(state): State<PlatformState>,
    Query(query): Query<AppRunsQuery>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    match app_runs::list_runs(
        pool,
        query.app.as_deref(),
        query.state.as_deref(),
        query.limit.unwrap_or(50),
    )
    .await
    {
        Ok(runs) => Json(json!({ "runs": runs })).into_response(),
        Err(err) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_runs_load_failed",
            err,
        ),
    }
}

async fn app_runs_show(
    State(state): State<PlatformState>,
    AxumPath(id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    match app_runs::get_run(pool, id).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "app_run_not_found", "run not found"),
        Err(err) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_load_failed",
            err,
        ),
    }
}

async fn app_run_events(
    State(state): State<PlatformState>,
    AxumPath(id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    match app_runs::list_events(pool, id).await {
        Ok(events) => Json(json!({ "events": events })).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "events_load_failed", err),
    }
}

fn authorize_write(
    state: &PlatformState,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    let Some(expected) = state.service_token.as_deref() else {
        return Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unconfigured",
            "AGENTHERO_SERVICE_TOKEN is unset",
        ));
    };
    let Some(actual) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing bearer token",
        ));
    };
    if actual != expected {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        ));
    }
    Ok(())
}

fn error_response(
    status: StatusCode,
    code: &str,
    detail: impl std::fmt::Display,
) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": code, "detail": detail.to_string() })),
    )
        .into_response()
}
