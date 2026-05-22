//! LaTeX → PDF compilation via the `tectonic` engine.
//!
//! Gated behind the `pdf` cargo feature because tectonic pulls in heavy native
//! dependencies and is not always desired in CI.

use anyhow::{Context, Result};

/// Compile a self-contained LaTeX document to PDF bytes.
pub fn latex_to_pdf(tex: &str) -> Result<Vec<u8>> {
    tectonic::latex_to_pdf(tex).context("tectonic::latex_to_pdf")
}
