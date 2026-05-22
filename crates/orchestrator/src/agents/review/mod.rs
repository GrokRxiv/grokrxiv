//! Review-agent modules.
//!
//! This directory owns review-role bindings and deterministic facts that are
//! injected into review prompts. Generic runner backends live in
//! [`crate::agents::runners`], and extraction-specific agents live in
//! [`grokrxiv_extraction::extraction`].

pub mod facts;

mod configured;

pub use configured::{build_agent, ConfiguredAgent};
