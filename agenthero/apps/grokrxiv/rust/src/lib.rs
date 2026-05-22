//! GrokRxiv DAGOps app implementation.
//!
//! This crate owns research-specific ingest, extraction, review, revision, and
//! publish behavior. AgentHero orchestration crates call it through app-level
//! APIs rather than hosting GrokRxiv modules inside the platform runtime.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use grokrxiv_extraction::extraction;
