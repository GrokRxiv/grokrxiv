//! GrokRxiv orchestrator: axum HTTP API + tokio supervisor.
//!
//! Modules are exposed as a library crate so integration tests and the binary
//! both share the same wiring.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agents;
pub mod arxiv_rate_limit;
pub mod cli;
pub mod cli_status;
pub mod config;
pub mod db;
pub mod doctor;
#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
pub mod ingest_pipeline;
pub mod routes;
pub mod runtime_config;
pub mod scheduler;
pub mod serve;
pub mod state;
pub mod stubs;
pub mod supervisor;
pub mod supervisor_runner;

pub use config::Config;
pub use runtime_config::{RuntimeConfig, RuntimeConfigOverrides};
pub use state::AppState;

/// Build the axum router for the orchestrator. Exposed so integration tests
/// can mount it against an in-process server.
pub fn router(state: AppState) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/preview", post(routes::preview::preview))
        .route("/ingest", post(routes::ingest::ingest))
        .route("/webhook/github", post(routes::webhook::github))
        // RPT2 Track I — operator-facing write endpoints reached via the
        // Next.js /api/v1/* proxy. The proxy enforces bearer auth; these
        // handlers run on the orchestrator's private network.
        .route("/internal/v1/review", post(routes::internal::review_post))
        .route(
            "/internal/v1/reviews/:id/approve",
            post(routes::internal::approve),
        )
        .route(
            "/internal/v1/reviews/:id/reject",
            post(routes::internal::reject),
        )
        .route(
            "/internal/v1/reviews/:id/render",
            post(routes::internal::render),
        )
        .route(
            "/internal/v1/reviews/:id/apply-revisions",
            post(routes::internal::apply_revisions),
        )
        .route(
            "/internal/v1/reviews/:id/verify",
            post(routes::internal::verify),
        )
        .route("/internal/v1/doctor", get(routes::internal::doctor))
        .with_state(state)
}
