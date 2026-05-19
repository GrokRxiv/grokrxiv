//! HTML renderer. Emits a single self-contained document with inline CSS so
//! the file is usable straight from the zip bundle.
//!
//! Math expressions are preserved with `\(...\)` / `\[...\]` delimiters for
//! client-side KaTeX rendering by the Next.js app.

use anyhow::{Context, Result};
use grokrxiv_schemas::{MetaReview, PaperExtract, Recommendation};
use minijinja::{context, Environment};
use once_cell::sync::Lazy;
use serde_json::json;

use crate::AgentRecord;

const TEMPLATE: &str = include_str!("../templates/review.html.jinja");

static ENV: Lazy<Environment<'static>> = Lazy::new(|| {
    let mut env = Environment::new();
    env.add_template("review.html", TEMPLATE)
        .expect("review.html template parses");
    env
});

/// Render the public HTML review.
pub fn render_html(
    meta: &MetaReview,
    paper: &PaperExtract,
    agents: &[AgentRecord],
) -> Result<String> {
    let agent_views: Vec<_> = agents
        .iter()
        .map(|a| {
            json!({
                "role": crate::role_slug(a.role),
                "model": a.model,
                "verifier_status": verifier_status_str(&a.verifier),
                "output_pretty": serde_json::to_string_pretty(&a.output).unwrap_or_default(),
            })
        })
        .collect();

    let tmpl = ENV
        .get_template("review.html")
        .expect("template registered");
    tmpl.render(context! {
        paper => paper,
        meta => meta,
        agents => agent_views,
        source_label => crate::paper_source_label(&paper.arxiv_id),
        source_url => crate::paper_source_url(&paper.arxiv_id),
        recommendation_label => recommendation_label(meta.recommendation),
        confidence_pct => (meta.confidence.clamp(0.0, 1.0) * 100.0).round() as i32,
    })
    .context("render review.html template")
}

fn recommendation_label(r: Recommendation) -> &'static str {
    match r {
        Recommendation::Accept => "Accept",
        Recommendation::MinorRevision => "Minor revision",
        Recommendation::MajorRevision => "Major revision",
        Recommendation::Reject => "Reject",
    }
}

fn verifier_status_str(v: &grokrxiv_schemas::VerifierResult) -> &'static str {
    match v.status {
        grokrxiv_schemas::VerifierStatus::Pass => "pass",
        grokrxiv_schemas::VerifierStatus::Warn => "warn",
        grokrxiv_schemas::VerifierStatus::Fail => "fail",
    }
}
