//! Top-level orchestration helper that chains arXiv metadata fetch → TeX
//! source (preferred) or PDF (fallback) extraction → [`PaperExtract`].
//!
//! Source-format policy (FP6.5 + RPT1, 2026-05-15):
//! - TeX is preferred; review reads from it when available
//! - PDF is the fallback when the arXiv source bundle is absent or unparseable
//! - PDF is always downloaded too (it's the viewable artifact)
//! - The resulting [`PaperExtract`] carries `source_format = "tex" | "pdf"`

use anyhow::Result;
use tracing::{info, warn};

use crate::arxiv::fetch_metadata;
use crate::download::{download_pdf, download_source};
use crate::extract::{extract_bibliography, pdf_to_text, split_sections};
use crate::tex::{parse_bundle, source_url};
use crate::types::{Citation, PaperExtract, Section};

/// End-to-end ingest from an arXiv id to a fully-populated [`PaperExtract`].
///
/// Strategy:
/// 1. Fetch Atom metadata (title/authors/abstract/category).
/// 2. Try to download the TeX source bundle. If it parses, use TeX-derived
///    title/abstract/sections/bibliography.
/// 3. If source is missing or unparseable, fall back to the PDF extraction
///    path. Same shape, lossier on math papers.
/// 4. PDF is always downloaded for the viewable artifact (currently
///    not persisted to `paper_assets` — see
///    `docs/ingest-tex-with-pdf-fallback-applied.md` for deferral notes).
pub async fn ingest(arxiv_id: &str) -> Result<PaperExtract> {
    let meta = fetch_metadata(arxiv_id).await?;
    let primary = meta.primary_category();

    // 1. Try TeX source first.
    let tex_result = match download_source(&source_url(arxiv_id)).await {
        Ok(bytes) => match parse_bundle(&bytes) {
            Ok(extract) => Some(extract),
            Err(e) => {
                warn!(error = %e, arxiv_id, "tex source parse failed; falling back to pdf");
                None
            }
        },
        Err(e) => {
            info!(error = %e, arxiv_id, "tex source unavailable; falling back to pdf");
            None
        }
    };

    // 2. Always grab the PDF (for fallback and/or viewable artifact).
    let pdf_bytes = match meta.pdf_url.as_deref() {
        Some(url) => match download_pdf(url).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warn!(error = %e, "pdf download failed");
                None
            }
        },
        None => None,
    };

    // 3. Build the extract, preferring TeX when present.
    let (title, abstract_text, sections, bibliography, source_format) = if let Some(t) = tex_result
    {
        info!(arxiv_id, "ingest source=tex");
        let title = if t.title.is_empty() { meta.title.clone() } else { t.title };
        let abstract_text = if t.abstract_text.is_empty() {
            meta.abstract_text.clone()
        } else {
            t.abstract_text
        };
        (
            title,
            abstract_text,
            t.sections,
            t.bibliography,
            Some("tex".to_string()),
        )
    } else {
        info!(arxiv_id, "ingest source=pdf");
        let (sections, bibliography) = pdf_extract(pdf_bytes.as_ref());
        (
            meta.title.clone(),
            meta.abstract_text.clone(),
            sections,
            bibliography,
            Some("pdf".to_string()),
        )
    };

    Ok(PaperExtract {
        arxiv_id: meta.arxiv_id,
        title,
        authors: meta.authors,
        abstract_: abstract_text,
        field: primary,
        sections,
        figures: Vec::new(),
        bibliography,
        source_format,
    })
}

fn pdf_extract(bytes: Option<&bytes::Bytes>) -> (Vec<Section>, Vec<Citation>) {
    let Some(bytes) = bytes else {
        return (Vec::new(), Vec::new());
    };
    match pdf_to_text(bytes) {
        Ok(text) => {
            let sections = split_sections(&text);
            let bib = extract_bibliography(&text);
            (sections, bib)
        }
        Err(e) => {
            warn!(error = %e, "pdf text extraction failed");
            (Vec::new(), Vec::new())
        }
    }
}
