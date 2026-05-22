//! Stubs for sibling pipeline crates (`ingest`, `render`, `verifier`,
//! `publisher`) so this binary compiles and runs before they ship.
//!
//! When you flip `--features full`, the matching `#[cfg(feature = "...")]`
//! branches below switch over to the real implementations.

#[cfg(not(feature = "grokrxiv-render"))]
use grokrxiv_schemas::Recommendation;
#[cfg(not(feature = "grokrxiv-ingest"))]
use grokrxiv_schemas::{Author, FigureRef, Section};
use grokrxiv_schemas::{MetaReview, PaperExtract};

/// Result returned from the local stub renderer.
#[derive(Debug, Clone)]
pub struct RenderedBundle {
    /// Rendered HTML (UTF-8).
    pub html: String,
    /// Rendered Markdown.
    pub markdown: String,
    /// Zipped bundle bytes (HTML + Markdown + JSON manifest).
    pub bundle: Vec<u8>,
}

/// Extract text from a PDF on disk.
///
/// * `stub-ingest`: returns a placeholder [`PaperExtract`] so the upstream
///   pipeline compiles and `/preview` can return a sample.
/// * `feature = "grokrxiv-ingest"`: delegates to the real ingest crate.
pub async fn pdf_to_text(_path: &std::path::Path) -> anyhow::Result<PaperExtract> {
    #[cfg(feature = "grokrxiv-ingest")]
    {
        // Real impl will live in grokrxiv_ingest::extract::pdf_to_text. The
        // signature it ultimately exposes may differ; this is intentionally a
        // small adapter so we can keep this orchestrator independent.
        Err(anyhow::anyhow!(
            "TODO(ingest): wire up grokrxiv_ingest::extract::pdf_to_text"
        ))
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        Ok(PaperExtract {
            arxiv_id: "stub.0000.00000".into(),
            title: "Sample paper (preview stub)".into(),
            authors: vec![Author {
                name: "Sample Author".into(),
                affiliation: None,
                email: None,
            }],
            abstract_: "Placeholder abstract extracted by the preview stub.".into(),
            field: Some("cs.AI".into()),
            sections: vec![Section {
                heading: "Introduction".into(),
                body_markdown: "Placeholder body.".into(),
            }],
            figures: vec![FigureRef {
                caption: "Figure 1".into(),
                page: 1,
                image_path: None,
            }],
            bibliography: vec![],
        })
    }
}

/// Render a meta-review + paper extract into HTML/Markdown/zip artifacts.
///
/// * `stub-render`: produces a trivial HTML page + Markdown + a one-file zip.
/// * `feature = "grokrxiv-render"`: delegates to the real render crate.
pub async fn render_bundle(
    meta: &MetaReview,
    paper: &PaperExtract,
) -> anyhow::Result<RenderedBundle> {
    #[cfg(feature = "grokrxiv-render")]
    let _ = (meta, paper);
    #[cfg(feature = "grokrxiv-render")]
    {
        Err(anyhow::anyhow!(
            "TODO(render): wire up grokrxiv_render::render_bundle"
        ))
    }
    #[cfg(not(feature = "grokrxiv-render"))]
    {
        let rec = match meta.recommendation {
            Recommendation::Accept => "Accept",
            Recommendation::MinorRevision => "Minor revision",
            Recommendation::MajorRevision => "Major revision",
            Recommendation::Reject => "Reject",
        };
        let html = format!(
            "<!doctype html><html><head><title>{title}</title></head>\
             <body><h1>{title}</h1><h2>Sample review (preview)</h2>\
             <p><strong>Recommendation:</strong> {rec}</p>\
             <h3>Summary</h3><p>{summary}</p></body></html>",
            title = html_escape(&paper.title),
            summary = html_escape(&meta.summary),
            rec = rec,
        );
        let markdown = format!(
            "# {title}\n\n**Recommendation:** {rec}\n\n## Summary\n\n{summary}\n",
            title = paper.title,
            rec = rec,
            summary = meta.summary,
        );
        // Cheap "zip" — for the stub we just embed the html+md into a zip
        // archive via the `zip` crate so the response shape matches the real
        // pipeline.
        let bundle = build_stub_zip(&html, &markdown)?;
        Ok(RenderedBundle {
            html,
            markdown,
            bundle,
        })
    }
}

#[cfg(not(feature = "grokrxiv-render"))]
fn build_stub_zip(html: &str, md: &str) -> anyhow::Result<Vec<u8>> {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut zw = zip_lite::SimpleZip::new(cursor);
    zw.add_file("review.html", html.as_bytes())?;
    zw.add_file("review.md", md.as_bytes())?;
    Ok(zw.finish()?.into_inner())
}

// Tiny zip writer used by the stub renderer so we don't take a dependency on
// the full `zip` crate from this crate.
#[cfg(not(feature = "grokrxiv-render"))]
mod zip_lite {
    use std::io::{Cursor, Result as IoResult, Write};

    /// Single-file stored-mode zip writer (no compression, sufficient for
    /// preview demos).
    pub struct SimpleZip {
        inner: Cursor<Vec<u8>>,
        entries: Vec<(
            String,
            u32, /*crc*/
            u32, /*size*/
            u32, /*offset*/
        )>,
    }

    impl SimpleZip {
        pub fn new(inner: Cursor<Vec<u8>>) -> Self {
            Self {
                inner,
                entries: Vec::new(),
            }
        }

        pub fn add_file(&mut self, name: &str, data: &[u8]) -> IoResult<()> {
            let offset = self.inner.position() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            let name_bytes = name.as_bytes();
            // Local file header: signature 0x04034b50
            self.inner.write_all(&0x04034b50u32.to_le_bytes())?;
            self.inner.write_all(&20u16.to_le_bytes())?; // version
            self.inner.write_all(&0u16.to_le_bytes())?; // flags
            self.inner.write_all(&0u16.to_le_bytes())?; // method = stored
            self.inner.write_all(&0u16.to_le_bytes())?; // mod time
            self.inner.write_all(&0u16.to_le_bytes())?; // mod date
            self.inner.write_all(&crc.to_le_bytes())?;
            self.inner.write_all(&size.to_le_bytes())?; // compressed
            self.inner.write_all(&size.to_le_bytes())?; // uncompressed
            self.inner
                .write_all(&(name_bytes.len() as u16).to_le_bytes())?;
            self.inner.write_all(&0u16.to_le_bytes())?; // extra field len
            self.inner.write_all(name_bytes)?;
            self.inner.write_all(data)?;
            self.entries.push((name.to_string(), crc, size, offset));
            Ok(())
        }

        pub fn finish(mut self) -> IoResult<Cursor<Vec<u8>>> {
            let central_dir_offset = self.inner.position() as u32;
            for (name, crc, size, offset) in &self.entries {
                let name_bytes = name.as_bytes();
                self.inner.write_all(&0x02014b50u32.to_le_bytes())?;
                self.inner.write_all(&20u16.to_le_bytes())?; // ver made by
                self.inner.write_all(&20u16.to_le_bytes())?; // ver needed
                self.inner.write_all(&0u16.to_le_bytes())?; // flags
                self.inner.write_all(&0u16.to_le_bytes())?; // method
                self.inner.write_all(&0u16.to_le_bytes())?; // mod time
                self.inner.write_all(&0u16.to_le_bytes())?; // mod date
                self.inner.write_all(&crc.to_le_bytes())?;
                self.inner.write_all(&size.to_le_bytes())?;
                self.inner.write_all(&size.to_le_bytes())?;
                self.inner
                    .write_all(&(name_bytes.len() as u16).to_le_bytes())?;
                self.inner.write_all(&0u16.to_le_bytes())?; // extra
                self.inner.write_all(&0u16.to_le_bytes())?; // comment
                self.inner.write_all(&0u16.to_le_bytes())?; // disk
                self.inner.write_all(&0u16.to_le_bytes())?; // internal attr
                self.inner.write_all(&0u32.to_le_bytes())?; // external attr
                self.inner.write_all(&offset.to_le_bytes())?;
                self.inner.write_all(name_bytes)?;
            }
            let central_dir_size = (self.inner.position() as u32) - central_dir_offset;
            // End of central directory
            self.inner.write_all(&0x06054b50u32.to_le_bytes())?;
            self.inner.write_all(&0u16.to_le_bytes())?; // disk
            self.inner.write_all(&0u16.to_le_bytes())?; // disk where cd starts
            self.inner
                .write_all(&(self.entries.len() as u16).to_le_bytes())?;
            self.inner
                .write_all(&(self.entries.len() as u16).to_le_bytes())?;
            self.inner.write_all(&central_dir_size.to_le_bytes())?;
            self.inner.write_all(&central_dir_offset.to_le_bytes())?;
            self.inner.write_all(&0u16.to_le_bytes())?; // comment len
            Ok(self.inner)
        }
    }

    fn crc32(data: &[u8]) -> u32 {
        // Standard zlib polynomial. Tiny implementation; the value is only
        // sanity-checked by zip readers, not security-critical.
        let mut table = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            table[i as usize] = c;
        }
        let mut crc = 0xffffffffu32;
        for &b in data {
            crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
        }
        crc ^ 0xffffffff
    }
}

#[cfg(not(feature = "grokrxiv-render"))]
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
