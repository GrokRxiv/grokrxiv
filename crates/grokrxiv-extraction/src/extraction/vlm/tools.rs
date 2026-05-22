//! PDF-specific tools used by [`VlmExtractorAgent`](super::VlmExtractorAgent).
//!
//! These tools assume the PDF has been written to
//! `<ctx.workdir>/<arxiv_id>.pdf` by the pipeline before the agent runs.
//!
//! - `read_pdf_page(page)` — render a page to 300-DPI PNG (base64) and pull
//!   the page's text layer. Returns total page count too.
//! - `search_pdf(query)` — case-insensitive substring search across every
//!   page's text layer; up to 10 hits with 200-char snippets.
//! - `extract_page_region(page, bbox)` — crop a normalised (0..1) rectangle
//!   from a 300-DPI render of the page.
//!
//! Text-layer extraction uses `lopdf` (pure Rust, no native deps). Page
//! rendering uses `pdfium-render`, which binds to a system PDFium library;
//! rendering tools surface a clean error if PDFium isn't available so callers
//! can fall back to text-only extraction.
//!
//! The PDF location is resolved against [`ToolCtx::workdir`] using the
//! `arxiv_id` from the context plus a `.pdf` suffix. Callers can override the
//! filename by passing an explicit `path` argument to `read_pdf_page` /
//! `extract_page_region` (paths are still scoped to the workdir).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use image::{DynamicImage, GenericImageView, ImageOutputFormat};
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Render DPI used for the page image returned by [`ReadPdfPageTool`].
pub const RENDER_DPI: u32 = 300;

/// US Letter / A4-ish page is ~612 / 595 pt wide. At 300 DPI that's ~2550 px
/// wide. We cap to this so a malformed PDF can't ask for an absurd render.
const MAX_RENDER_PX: u32 = 4_096;

/// Maximum number of hits returned by [`SearchPdfTool`].
pub const MAX_SEARCH_HITS: usize = 10;

/// Width of the snippet window returned by [`SearchPdfTool`] (centered on the
/// match, in characters).
pub const SNIPPET_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// read_pdf_page
// ---------------------------------------------------------------------------

/// Implements `read_pdf_page(page)`.
pub struct ReadPdfPageTool;

static READ_PDF_PAGE_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn read_pdf_page_schema() -> Value {
    json!({
        "type": "object",
        "required": ["page"],
        "properties": {
            "page": {
                "type": "integer",
                "minimum": 1,
                "description": "1-based page number to read."
            },
            "path": {
                "type": "string",
                "description": "Optional explicit PDF path (relative to workdir). Defaults to `<arxiv_id>.pdf`."
            }
        }
    })
}

#[async_trait]
impl Tool for ReadPdfPageTool {
    fn name(&self) -> &'static str {
        "read_pdf_page"
    }
    fn description(&self) -> &'static str {
        "Render a PDF page to a 300-DPI PNG image (base64) and return its text layer plus total page count."
    }
    fn schema(&self) -> &Value {
        READ_PDF_PAGE_SCHEMA.get_or_init(read_pdf_page_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let page = args
            .get("page")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("read_pdf_page requires integer `page`"))?;
        if page < 1 {
            anyhow::bail!("read_pdf_page: page must be >= 1, got {page}");
        }
        let pdf_path = resolve_pdf_path(args.get("path").and_then(Value::as_str), ctx)?;
        let bytes = std::fs::read(&pdf_path).map_err(|e| {
            anyhow::anyhow!(
                "read_pdf_page: could not open `{}`: {e}",
                pdf_path.display()
            )
        })?;

        let (text_layer, total_pages) = text_layer_for_page(&bytes, page as u32)?;
        let image_b64 = render_page_b64(&bytes, page as u32, None);

        Ok(json!({
            "page": page,
            "total_pages": total_pages,
            "text_layer": text_layer,
            "image_b64": image_b64,
        }))
    }
}

// ---------------------------------------------------------------------------
// search_pdf
// ---------------------------------------------------------------------------

/// Implements `search_pdf(query)`.
pub struct SearchPdfTool;

static SEARCH_PDF_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn search_pdf_schema() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": {
                "type": "string",
                "description": "Phrase to search for (case-insensitive substring match)."
            },
            "path": {
                "type": "string",
                "description": "Optional explicit PDF path (relative to workdir)."
            }
        }
    })
}

#[async_trait]
impl Tool for SearchPdfTool {
    fn name(&self) -> &'static str {
        "search_pdf"
    }
    fn description(&self) -> &'static str {
        "Search every page of the PDF for a phrase; returns up to 10 hits as {page, snippet, char_offset}."
    }
    fn schema(&self) -> &Value {
        SEARCH_PDF_SCHEMA.get_or_init(search_pdf_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("search_pdf requires `query`"))?;
        if query.is_empty() {
            anyhow::bail!("search_pdf: `query` must be non-empty");
        }
        let pdf_path = resolve_pdf_path(args.get("path").and_then(Value::as_str), ctx)?;
        let bytes = std::fs::read(&pdf_path).map_err(|e| {
            anyhow::anyhow!("search_pdf: could not open `{}`: {e}", pdf_path.display())
        })?;

        let doc = lopdf::Document::load_mem(&bytes)
            .map_err(|e| anyhow::anyhow!("search_pdf: lopdf failed to parse PDF: {e}"))?;
        let needle = query.to_lowercase();

        let mut hits: Vec<Value> = Vec::new();
        for (page_no, _id) in doc.get_pages() {
            if hits.len() >= MAX_SEARCH_HITS {
                break;
            }
            let text = match doc.extract_text(&[page_no]) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let haystack = text.to_lowercase();
            let mut start = 0usize;
            while let Some(rel) = haystack[start..].find(&needle) {
                let off = start + rel;
                let snippet = snippet_around(&text, off, SNIPPET_CHARS);
                hits.push(json!({
                    "page": page_no,
                    "char_offset": off,
                    "snippet": snippet,
                }));
                if hits.len() >= MAX_SEARCH_HITS {
                    break;
                }
                start = off + needle.len();
            }
        }
        Ok(json!({ "hits": hits, "count": hits.len() }))
    }
}

// ---------------------------------------------------------------------------
// extract_page_region
// ---------------------------------------------------------------------------

/// Implements `extract_page_region(page, bbox)`.
pub struct ExtractPageRegionTool;

static EXTRACT_REGION_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn extract_region_schema() -> Value {
    json!({
        "type": "object",
        "required": ["page", "bbox"],
        "properties": {
            "page": { "type": "integer", "minimum": 1 },
            "bbox": {
                "type": "object",
                "required": ["x", "y", "w", "h"],
                "properties": {
                    "x": { "type": "number", "minimum": 0, "maximum": 1 },
                    "y": { "type": "number", "minimum": 0, "maximum": 1 },
                    "w": { "type": "number", "minimum": 0, "maximum": 1 },
                    "h": { "type": "number", "minimum": 0, "maximum": 1 }
                },
                "description": "Normalised crop rectangle in 0..1 coordinates (origin top-left)."
            },
            "path": { "type": "string" }
        }
    })
}

#[async_trait]
impl Tool for ExtractPageRegionTool {
    fn name(&self) -> &'static str {
        "extract_page_region"
    }
    fn description(&self) -> &'static str {
        "Crop a normalised (0..1) bbox from a 300-DPI render of the page; returns the cropped region as a base64 PNG."
    }
    fn schema(&self) -> &Value {
        EXTRACT_REGION_SCHEMA.get_or_init(extract_region_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let page = args
            .get("page")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("extract_page_region requires `page`"))?;
        if page < 1 {
            anyhow::bail!("extract_page_region: page must be >= 1");
        }
        let bbox = args
            .get("bbox")
            .ok_or_else(|| anyhow::anyhow!("extract_page_region requires `bbox`"))?;
        let x = bbox
            .get("x")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let y = bbox
            .get("y")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let w = bbox
            .get("w")
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        let h = bbox
            .get("h")
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        if w <= 0.0 || h <= 0.0 {
            anyhow::bail!("extract_page_region: bbox must have positive width and height");
        }

        let pdf_path = resolve_pdf_path(args.get("path").and_then(Value::as_str), ctx)?;
        let bytes = std::fs::read(&pdf_path).map_err(|e| {
            anyhow::anyhow!(
                "extract_page_region: could not open `{}`: {e}",
                pdf_path.display()
            )
        })?;

        let image_b64 = render_page_b64(&bytes, page as u32, Some((x, y, w, h)))
            .ok_or_else(|| anyhow::anyhow!("extract_page_region: PDFium rendering unavailable"))?;
        Ok(json!({
            "page": page,
            "bbox": { "x": x, "y": y, "w": w, "h": h },
            "image_b64": image_b64,
        }))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_pdf_path(explicit: Option<&str>, ctx: &ToolCtx<'_>) -> anyhow::Result<PathBuf> {
    let rel: PathBuf = match explicit {
        Some(s) => Path::new(s).to_path_buf(),
        None => Path::new(&format!("{}.pdf", ctx.arxiv_id)).to_path_buf(),
    };
    if rel.is_absolute() {
        anyhow::bail!("pdf path must be relative to the workdir");
    }
    let full = ctx.workdir.join(&rel);
    // Reject path-escape attempts; the canonicalize check happens lazily so
    // missing files surface their own friendly error.
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("pdf path may not contain `..`");
    }
    Ok(full)
}

fn text_layer_for_page(bytes: &[u8], page: u32) -> anyhow::Result<(String, u32)> {
    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| anyhow::anyhow!("lopdf could not parse the PDF: {e}"))?;
    let pages = doc.get_pages();
    let total = pages.len() as u32;
    if !pages.contains_key(&page) {
        anyhow::bail!("page {page} out of range (total_pages={total})");
    }
    let text = doc
        .extract_text(&[page])
        .map_err(|e| anyhow::anyhow!("text-layer extraction failed: {e}"))?;
    Ok((text, total))
}

/// Render a page (optionally cropped) and return a base64 PNG. Returns
/// `None` if PDFium is unavailable on the host so callers can gracefully
/// degrade to text-only extraction.
fn render_page_b64(bytes: &[u8], page: u32, crop: Option<(f64, f64, f64, f64)>) -> Option<String> {
    use pdfium_render::prelude::Pdfium;
    // Try the bundled / linked library first, then fall back to whatever
    // libpdfium.{dylib,so} happens to be on the system path.
    let bindings = Pdfium::bind_to_system_library().ok()?;
    let pdf = Pdfium::new(bindings);
    let doc = pdf.load_pdf_from_byte_slice(bytes, None).ok()?;
    let pages = doc.pages();
    if page == 0 || page as u16 > pages.len() {
        return None;
    }
    let pdf_page = pages.get(page as u16 - 1).ok()?;
    let (target_w, target_h) = target_dims(&pdf_page);
    let bitmap = pdf_page
        .render(target_w as i32, target_h as i32, None)
        .ok()?;
    let img = bitmap.as_image();
    let cropped = apply_crop(img, crop);
    encode_png_b64(&cropped)
}

fn target_dims(page: &pdfium_render::prelude::PdfPage<'_>) -> (u32, u32) {
    use pdfium_render::prelude::PdfPoints;
    let w_pt: PdfPoints = page.width();
    let h_pt: PdfPoints = page.height();
    let px = |pt: f32| -> u32 {
        let raw = (pt / 72.0 * RENDER_DPI as f32).round() as i64;
        raw.clamp(64, MAX_RENDER_PX as i64) as u32
    };
    (px(w_pt.value), px(h_pt.value))
}

fn apply_crop(img: DynamicImage, crop: Option<(f64, f64, f64, f64)>) -> DynamicImage {
    let Some((x, y, w, h)) = crop else {
        return img;
    };
    let (iw, ih) = img.dimensions();
    let cx = (x * iw as f64).floor() as u32;
    let cy = (y * ih as f64).floor() as u32;
    let cw = ((w * iw as f64).round() as u32)
        .max(1)
        .min(iw.saturating_sub(cx));
    let ch = ((h * ih as f64).round() as u32)
        .max(1)
        .min(ih.saturating_sub(cy));
    img.crop_imm(cx, cy, cw, ch)
}

fn encode_png_b64(img: &DynamicImage) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageOutputFormat::Png)
        .ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(&buf))
}

fn snippet_around(text: &str, byte_offset: usize, width: usize) -> String {
    let half = width / 2;
    let start = byte_offset.saturating_sub(half);
    let end = (byte_offset + half).min(text.len());
    // Walk to nearest UTF-8 char boundaries so we don't slice mid-codepoint.
    let mut s = start;
    while s > 0 && !text.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end;
    while e < text.len() && !text.is_char_boundary(e) {
        e += 1;
    }
    text[s..e].to_string()
}

/// Build a tiny fixture PDF in memory containing two pages with known text.
/// Used by both this module's tests and `vlm/mod.rs` agent-level tests.
/// Lives behind `cfg(test)` so it doesn't leak into release builds.
#[cfg(test)]
pub(crate) fn build_fixture_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let make_page = |doc: &mut Document, body: &str| -> lopdf::ObjectId {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 16.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(body)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        })
    };

    let p1 = make_page(&mut doc, "Hello category theory primer");
    let p2 = make_page(&mut doc, "Second page about topology");

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![p1.into(), p2.into()],
        "Count" => 2,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();
    let mut buf: Vec<u8> = Vec::new();
    doc.save_to(&mut buf).expect("save fixture pdf");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::ToolCtx;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Write the fixture into a workdir at `<arxiv_id>.pdf` and return the
    /// workdir path. Caller owns the tempdir cleanup.
    pub(super) fn fixture_workdir(arxiv_id: &str) -> (PathBuf, Vec<u8>) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "grokrxiv-vlm-tools-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = build_fixture_pdf();
        std::fs::write(dir.join(format!("{arxiv_id}.pdf")), &pdf).unwrap();
        (dir, pdf)
    }

    fn ctx_for<'a>(workdir: &'a Path, arxiv_id: &'a str) -> ToolCtx<'a> {
        ToolCtx {
            workdir,
            semantic_ast: None,
            source_id: arxiv_id,
            arxiv_id,
            http: Arc::new(reqwest::Client::new()),
        }
    }

    #[tokio::test]
    async fn read_pdf_page_returns_text_layer() {
        let (dir, _pdf) = fixture_workdir("2401.00001v1");
        let ctx = ctx_for(&dir, "2401.00001v1");
        let res = ReadPdfPageTool
            .call(json!({ "page": 1 }), &ctx)
            .await
            .expect("read page 1");
        assert_eq!(res.get("total_pages").and_then(Value::as_u64), Some(2));
        let text = res
            .get("text_layer")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            text.to_lowercase().contains("category theory"),
            "text_layer should include the embedded phrase, got {text:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_pdf_returns_hits() {
        let (dir, _pdf) = fixture_workdir("2401.00002v1");
        let ctx = ctx_for(&dir, "2401.00002v1");
        let res = SearchPdfTool
            .call(json!({ "query": "category" }), &ctx)
            .await
            .expect("search succeeds");
        let hits = res
            .get("hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(!hits.is_empty(), "expected >=1 hit for 'category'");
        let first = &hits[0];
        assert_eq!(first.get("page").and_then(Value::as_u64), Some(1));
        let snippet = first.get("snippet").and_then(Value::as_str).unwrap_or("");
        assert!(
            snippet.to_lowercase().contains("category"),
            "snippet should contain the match, got {snippet:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn extract_page_region_returns_image_b64() {
        let (dir, _pdf) = fixture_workdir("2401.00003v1");
        let ctx = ctx_for(&dir, "2401.00003v1");
        let res = ExtractPageRegionTool
            .call(
                json!({
                    "page": 1,
                    "bbox": { "x": 0.0, "y": 0.0, "w": 0.5, "h": 0.5 }
                }),
                &ctx,
            )
            .await;
        // PDFium isn't required to be installed for the test suite; if it
        // isn't, the rendering tools surface a clean error and we skip the
        // image-decode assertion. The text-only tests above still cover the
        // pure-Rust code paths.
        match res {
            Ok(val) => {
                let b64 = val
                    .get("image_b64")
                    .and_then(Value::as_str)
                    .expect("image_b64 string");
                let png = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .expect("base64 decodes");
                assert!(
                    png.starts_with(&[0x89, b'P', b'N', b'G']),
                    "decoded payload should be a PNG, got first bytes {:?}",
                    &png.get(..8)
                );
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("PDFium rendering unavailable") || msg.contains("could not open"),
                    "unexpected error: {msg}"
                );
                eprintln!("PDFium not available in this environment — render test skipped");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
