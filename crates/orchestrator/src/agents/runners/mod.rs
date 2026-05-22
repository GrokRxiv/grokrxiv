//! Runner backends.
//!
//! Each runner implements [`AgentRunner`] and answers the
//! question: "how is this role's work actually executed?"
//!
//! - [`api::ApiRunner`] — direct LLM provider API
//! - [`cli::CliRunner`] — local subprocess for review agents by default
//! - [`cloud::CloudRunner`] — durable cloud workflow (Vercel / E2B)
//! - [`local_inference::LocalInferenceRunner`] — Ollama / LiteLLM gateway
//!
//! Container isolation is NOT a runner — it's a [`super::types::SandboxPolicy`]
//! applied UNDER `cli` or `local_inference`. See [`super::sandbox`].

pub mod api;
pub mod cli;
pub mod cloud;
pub mod local_inference;
pub mod traits;

pub use traits::AgentRunner;
