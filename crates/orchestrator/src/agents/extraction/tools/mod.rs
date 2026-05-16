//! Shared core toolkit for every extraction agent.
//!
//! Each tool is in its own file. The implementations are deliberately small,
//! deterministic, and side-effect-light — the LLM owns the decision-making,
//! not the tools.

pub mod arxiv_lookup;
pub mod crossref_lookup;
pub mod list_files;
pub mod query_ast;
pub mod read_file;
pub mod submit;
