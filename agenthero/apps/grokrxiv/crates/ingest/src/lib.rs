//! GrokRxiv ingest crate: arXiv puller + PDF/LaTeX text extraction.
//!
//! Public surface is small and async: [`pipeline::ingest`] is the one-shot
//! entry point that returns a [`PaperExtract`].

pub mod arxiv;
pub mod download;
pub mod extract;
pub mod listing;
pub mod pipeline;
pub mod source;
pub mod tex;
pub mod types;

pub use arxiv::{fetch_metadata, parse_atom, ArxivMeta};
pub use download::{download_pdf, download_source};
pub use extract::{
    extract_bibliography, normalize_pdf_text, pdf_to_text, split_sections, NormalizedPdfText,
};
pub use listing::{
    fetch_list_page, fetch_listing, IngestError, ALL_CATEGORIES, DEFAULT_ACTIVE_CATEGORIES,
};
pub use pipeline::{ingest, ingest_staged, DeterministicIngest};
pub use source::{
    prepare_git_repo_source, prepare_local_file_source, prepare_review_source,
    scan_git_repo_corpus, CorpusManuscriptCandidate, CorpusScanOptions, InferredSubject,
    LocalSourceFormat, PreparedReviewSource, ReviewSourceSpec, SourceIdentity, SourceKind,
};
pub use tex::{
    bibliography_from_bundle_bytes, extract_main_tex_source, parse_bundle, source_url,
    MainTexSource, TexBodyProducer, TexExtract,
};
pub use types::*;
