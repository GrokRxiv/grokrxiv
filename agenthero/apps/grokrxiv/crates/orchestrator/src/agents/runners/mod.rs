//! Runner backends.
//!
//! Each runner implements [`AgentRunner`] and answers the
//! question: "how is this role's work actually executed?"
//!
//! - [`api::ApiRunner`] — direct LLM provider API
//! - [`cli::CliRunner`] — local subprocess for review agents by default
//!
//! Container isolation is NOT a runner — it's a [`super::types::SandboxPolicy`]
//! applied under supported runners. See [`super::sandbox`].

pub mod api;
pub mod cli;
pub mod traits;

pub use traits::AgentRunner;
