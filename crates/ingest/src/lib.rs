//! GrokRxiv ingest crate: arXiv puller + PDF/LaTeX text extraction.
//!
//! Public surface is small and async: [`pipeline::ingest`] is the one-shot
//! entry point that returns a [`PaperExtract`].

pub mod arxiv;
pub mod download;
pub mod extract;
pub mod listing;
pub mod pipeline;
pub mod tex;
pub mod types;

pub use arxiv::{fetch_metadata, parse_atom, ArxivMeta};
pub use download::{download_pdf, download_source};
pub use extract::{extract_bibliography, pdf_to_text, split_sections};
pub use listing::{fetch_listing, IngestError, ALL_CATEGORIES, DEFAULT_ACTIVE_CATEGORIES};
pub use pipeline::ingest;
pub use tex::{parse_bundle, source_url, TexExtract};
pub use types::*;
