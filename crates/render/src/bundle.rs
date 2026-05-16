//! Build the canonical `review.zip` bundle. Layout:
//!
//! ```text
//! review.html
//! review.md
//! review.tex
//! review.pdf            (optional)
//! metadata.json
//! agents/<role>.json    (one per agent)
//! ```

use anyhow::{Context, Result};
use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// Pack the supplied artifacts into an in-memory zip and return the bytes.
pub fn build_zip(
    html: &str,
    md: &str,
    tex: &str,
    pdf: Option<&[u8]>,
    agent_jsons: &[(String, Vec<u8>)],
    metadata: &serde_json::Value,
) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(8192);
    {
        let cursor = Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        write_entry(&mut zip, "review.html", html.as_bytes(), opts)?;
        write_entry(&mut zip, "review.md", md.as_bytes(), opts)?;
        write_entry(&mut zip, "review.tex", tex.as_bytes(), opts)?;
        if let Some(pdf_bytes) = pdf {
            write_entry(&mut zip, "review.pdf", pdf_bytes, opts)?;
        }

        let meta_bytes = serde_json::to_vec_pretty(metadata).context("serialize metadata.json")?;
        write_entry(&mut zip, "metadata.json", &meta_bytes, opts)?;

        for (path, bytes) in agent_jsons {
            write_entry(&mut zip, path, bytes, opts)?;
        }

        zip.finish().context("finalize zip")?;
    }
    Ok(buf)
}

fn write_entry(
    zip: &mut ZipWriter<Cursor<&mut Vec<u8>>>,
    name: &str,
    data: &[u8],
    opts: SimpleFileOptions,
) -> Result<()> {
    zip.start_file(name, opts)
        .with_context(|| format!("zip start_file {name}"))?;
    zip.write_all(data)
        .with_context(|| format!("zip write {name}"))?;
    Ok(())
}
