//! GrokRxiv render crate: turn a [`MetaReview`] + [`PaperExtract`] into HTML,
//! Markdown, LaTeX, optional PDF, and a self-contained zip bundle.
//!
//! Rendered artifacts intentionally omit any legal/disclaimer text — the
//! single source of truth lives on the web `/legal` page. See
//! [`PUBLIC_DISCLAIMER`] (reserved/empty) and the negative-assertion
//! integration tests in `crates/render/tests/render.rs`.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod html;
pub mod latex;
pub mod md;

#[cfg(feature = "pdf")]
pub mod pdf;

use grokrxiv_schemas::{AgentRole, VerifierResult};
use serde::{Deserialize, Serialize};

pub use bundle::build_zip;
pub use html::render_html;
pub use latex::render_latex;
pub use md::render_markdown;

/// Reserved for the dedicated `/legal` web page only — intentionally empty
/// so the constant cannot accidentally render into headers, footers,
/// banners, review bodies, or PR bodies. Kept as a public symbol so
/// downstream tests can lock the policy (see crates/render/tests/render.rs).
pub const PUBLIC_DISCLAIMER: &str = "";

/// Per-agent record bundled into the final artifacts. Mirrors the
/// `review_agents` table without the database-only columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    /// Specialist role this run belongs to.
    pub role: AgentRole,
    /// LLM model identifier (e.g. `claude-opus-4-7`).
    pub model: String,
    /// Raw agent output as JSON; renderers display it pretty-printed.
    pub output: serde_json::Value,
    /// Verifier outcome for this run.
    pub verifier: VerifierResult,
}

impl AgentRecord {
    /// Stable filename for this record inside the zip bundle.
    pub fn filename(&self) -> String {
        format!("agents/{}.json", role_slug(self.role))
    }
}

/// Stable snake_case label for an [`AgentRole`].
pub fn role_slug(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Summary => "summary",
        AgentRole::TechnicalCorrectness => "technical_correctness",
        AgentRole::Novelty => "novelty",
        AgentRole::Reproducibility => "reproducibility",
        AgentRole::Citation => "citation",
        AgentRole::MetaReviewer => "meta_reviewer",
    }
}
