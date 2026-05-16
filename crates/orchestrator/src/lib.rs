//! GrokRxiv orchestrator: axum HTTP API + tokio supervisor.
//!
//! Modules are exposed as a library crate so integration tests and the binary
//! both share the same wiring.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod arxiv_rate_limit;
pub mod cli;
pub mod config;
pub mod db;
pub mod routes;
pub mod scheduler;
pub mod serve;
pub mod state;
pub mod stubs;
pub mod supervisor;

pub use config::Config;
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
        .with_state(state)
}
