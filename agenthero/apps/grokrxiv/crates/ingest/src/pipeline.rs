//! Top-level deterministic ingest helpers — Stages 1 + 2 of the RPT3
//! 8-stage pipeline. These are the **deterministic** halves; the orchestrator
//! crate owns Stages 3–8 (extraction agents + persistence) because they
//! require a DB pool, runner registry, and the storage SDK.
//!
//! Source-format policy:
//! - TeX is preferred; review reads from it when available.
//! - PDF is the fallback when the arXiv source bundle is absent or
//!   unparseable.
//! - PDF + source tarball bytes are always returned so the orchestrator can
//!   route them to Tier-2 storage in Stage 8.
//!
//! Back-compat: [`ingest`] still returns a `PaperExtract` so legacy callers
//! (the M1 smoke test, the review-only `run_review_for_paper_full` re-ingest
//! path) keep working. [`ingest_staged`] is the new entry point for the
//! orchestrator's full pipeline.

use anyhow::Result;
use bytes::Bytes;
use serde_json::Value;
use tracing::{info, warn};

use crate::arxiv::{fetch_metadata, ArxivMeta};
use crate::download::{download_pdf, download_source};
use crate::extract::{extract_bibliography, normalize_pdf_text, pdf_to_text, split_sections};
use crate::tex::{parse_bundle, source_url, TexBodyProducer};
use crate::types::{Citation, PaperExtract, Section};

/// Output of Stages 1 + 2 — everything an orchestrator needs to drive the
/// extraction-agent fan-out and the storage Stage-8 persist.
pub struct DeterministicIngest {
    /// arXiv Atom metadata (title/authors/abstract/category, plus pdf_url).
    pub meta: ArxivMeta,
    /// Built `PaperExtract` (title/abstract/sections/bibliography). The
    /// section bodies are the reviewable Markdown the review path consumes.
    pub extract: PaperExtract,
    /// Raw PDF bytes (always fetched — it's the archival viewable artifact).
    /// `None` only when the upstream PDF endpoint failed; pipeline continues.
    pub pdf_bytes: Option<Bytes>,
    /// Raw TeX tar.gz bytes when an arXiv source bundle was available.
    /// `None` for PDF-only papers — Stage 3 (`VlmExtractorAgent`) takes over.
    pub source_tarball: Option<Bytes>,
    /// Optional LaTeXML-derived semantic AST. `Some` only when Stage 2 ran the
    /// opt-in LaTeXML pipeline successfully. The orchestrator hands this to
    /// extraction agents via `ExtractionContext.semantic_ast`.
    pub semantic_ast: Option<Value>,
    /// Source-to-body converter used for TeX bundles; `None` on PDF fallback.
    pub tex_body_producer: Option<TexBodyProducer>,
}

/// Deterministic Stages 1 + 2: arXiv metadata fetch → TeX source (preferred)
/// or PDF (fallback) → [`DeterministicIngest`].
///
/// Stage 3+ extraction agents and Stage 8 persistence live in the GrokRxiv
/// app runtime crate (see `grokrxiv_app_runtime::ingest_pipeline`).
pub async fn ingest_staged(arxiv_id: &str) -> Result<DeterministicIngest> {
    let meta = fetch_metadata(arxiv_id).await?;
    let primary = meta.primary_category();

    // 1. Try TeX source first (Stage 1: source acquisition).
    let (tex_extract, source_tarball) = match download_source(&source_url(arxiv_id)).await {
        Ok(bytes) => match parse_bundle(&bytes).await {
            Ok(extract) => (Some(extract), Some(bytes)),
            Err(e) => {
                warn!(error = %e, arxiv_id, "tex source parse failed; falling back to pdf");
                (None, None)
            }
        },
        Err(e) => {
            info!(error = %e, arxiv_id, "tex source unavailable; falling back to pdf");
            (None, None)
        }
    };

    // 2. Always grab the PDF (Stage 1: archival artifact).
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

    // 3. Build the extract, preferring TeX when present (Stage 2).
    let (extract, semantic_ast, tex_body_producer) = if let Some(t) = tex_extract {
        info!(arxiv_id, "ingest source=tex");
        let body_producer = t.body_producer;
        let title = if t.title.is_empty() {
            meta.title.clone()
        } else {
            t.title
        };
        let abstract_text = if t.abstract_text.is_empty() {
            meta.abstract_text.clone()
        } else {
            t.abstract_text
        };
        let extract = PaperExtract {
            arxiv_id: meta.arxiv_id.clone(),
            title,
            authors: meta.authors.clone(),
            abstract_: abstract_text,
            field: primary.clone(),
            sections: t.sections,
            figures: Vec::new(),
            bibliography: t.bibliography,
            source_format: Some("tex".to_string()),
        };
        (extract, t.semantic_ast, Some(body_producer))
    } else {
        info!(arxiv_id, "ingest source=pdf");
        let (sections, bibliography) = pdf_extract(pdf_bytes.as_ref());
        let extract = PaperExtract {
            arxiv_id: meta.arxiv_id.clone(),
            title: meta.title.clone(),
            authors: meta.authors.clone(),
            abstract_: meta.abstract_text.clone(),
            field: primary,
            sections,
            figures: Vec::new(),
            bibliography,
            source_format: Some("pdf".to_string()),
        };
        (extract, None, None)
    };

    Ok(DeterministicIngest {
        meta,
        extract,
        pdf_bytes,
        source_tarball,
        semantic_ast,
        tex_body_producer,
    })
}

/// Legacy entry point: returns only the [`PaperExtract`]. Preserved so the
/// M1 smoke test and the review-only re-ingest path in the supervisor keep
/// working unchanged.
pub async fn ingest(arxiv_id: &str) -> Result<PaperExtract> {
    let staged = ingest_staged(arxiv_id).await?;
    Ok(staged.extract)
}

fn pdf_extract(bytes: Option<&Bytes>) -> (Vec<Section>, Vec<Citation>) {
    let Some(bytes) = bytes else {
        return (Vec::new(), Vec::new());
    };
    match pdf_to_text(bytes) {
        Ok(text) => {
            let normalized = normalize_pdf_text(&text);
            let sections = split_sections(&normalized.text);
            let bib = extract_bibliography(&normalized.text);
            (sections, bib)
        }
        Err(e) => {
            warn!(error = %e, "pdf text extraction failed");
            (Vec::new(), Vec::new())
        }
    }
}
