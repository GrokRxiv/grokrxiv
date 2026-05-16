//! Top-level orchestration helper that chains arXiv metadata fetch → PDF
//! download → text/section/bibliography extraction.

use anyhow::Result;
use tracing::warn;

use crate::arxiv::fetch_metadata;
use crate::download::download_pdf;
use crate::extract::{extract_bibliography, pdf_to_text, split_sections};
use crate::types::PaperExtract;

/// End-to-end ingest from an arXiv id to a fully-populated [`PaperExtract`].
///
/// On PDF download / parse failure we log a warning but still return a partial
/// extract built from the arXiv metadata alone — review agents can still run
/// on the abstract.
pub async fn ingest(arxiv_id: &str) -> Result<PaperExtract> {
    let meta = fetch_metadata(arxiv_id).await?;
    let primary = meta.primary_category();

    let (sections, bibliography) = if let Some(pdf_url) = meta.pdf_url.as_deref() {
        match download_pdf(pdf_url).await {
            Ok(bytes) => match pdf_to_text(&bytes) {
                Ok(text) => {
                    let sections = split_sections(&text);
                    let bib = extract_bibliography(&text);
                    (sections, bib)
                }
                Err(e) => {
                    warn!(error = %e, "pdf text extraction failed");
                    (Vec::new(), Vec::new())
                }
            },
            Err(e) => {
                warn!(error = %e, "pdf download failed");
                (Vec::new(), Vec::new())
            }
        }
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(PaperExtract {
        arxiv_id: meta.arxiv_id,
        title: meta.title,
        authors: meta.authors,
        abstract_: meta.abstract_text,
        field: primary,
        sections,
        figures: Vec::new(),
        bibliography,
    })
}
