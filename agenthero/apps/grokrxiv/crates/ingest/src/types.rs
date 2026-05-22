//! Re-exports of the shared schema types used by this crate.
//!
//! All wire-level structs (paper, authors, sections, figures, citations) live
//! in `grokrxiv-schemas` so every pipeline stage agrees on the contract.

pub use grokrxiv_schemas::{Author, Citation, FigureRef, PaperExtract, Section};
