//! Markdown renderer. Mirrors the HTML structure but in plain Markdown so
//! the artifact remains portable.

use grokrxiv_schemas::{MetaReview, PaperExtract, Recommendation};

use crate::AgentRecord;

/// Render the public Markdown review.
pub fn render_markdown(meta: &MetaReview, paper: &PaperExtract, agents: &[AgentRecord]) -> String {
    let mut out = String::with_capacity(4096);

    // Prominent leading disclaimer as a blockquote.
    // Disclaimer suppressed — see crates/render/src/lib.rs::PUBLIC_DISCLAIMER.

    out.push_str(&format!("# {}\n\n", paper.title));
    out.push_str(&format!(
        "GrokRxiv review of [arXiv:{0}](https://arxiv.org/abs/{0})",
        paper.arxiv_id
    ));
    if let Some(field) = &paper.field {
        out.push_str(&format!(" · `{field}`"));
    }
    out.push_str("\n\n");

    if !paper.authors.is_empty() {
        let authors: Vec<_> = paper.authors.iter().map(|a| a.name.as_str()).collect();
        out.push_str(&format!("_Authors_: {}\n\n", authors.join(", ")));
    }

    out.push_str("## TL;DR\n\n");
    out.push_str(&meta.summary);
    out.push_str("\n\n");
    out.push_str(&format!(
        "_Recommendation_: **{}** · _Confidence_: {:.0}%\n\n",
        recommendation_label(meta.recommendation),
        meta.confidence.clamp(0.0, 1.0) * 100.0
    ));

    out.push_str("## Strengths\n\n");
    for s in &meta.strengths {
        out.push_str(&format!("- {s}\n"));
    }
    out.push('\n');

    out.push_str("## Weaknesses\n\n");
    for w in &meta.weaknesses {
        out.push_str(&format!("- {w}\n"));
    }
    out.push('\n');

    out.push_str("## Open Questions\n\n");
    for q in &meta.questions {
        out.push_str(&format!("- {q}\n"));
    }
    out.push('\n');

    out.push_str("## Per-Agent Reviews\n\n");
    for agent in agents {
        out.push_str(&format!(
            "### {} (`{}`) — status: `{}`\n\n",
            crate::role_slug(agent.role),
            agent.model,
            match agent.verifier.status {
                grokrxiv_schemas::VerifierStatus::Pass => "pass",
                grokrxiv_schemas::VerifierStatus::Warn => "warn",
                grokrxiv_schemas::VerifierStatus::Fail => "fail",
            }
        ));
        out.push_str("```json\n");
        out.push_str(
            &serde_json::to_string_pretty(&agent.output).unwrap_or_else(|_| "{}".to_string()),
        );
        out.push_str("\n```\n\n");
    }

    out.push_str("## Corrections\n\n");
    // The corrections section is wired by task #12. The marker below lets the
    // future renderer find this slot and replace the placeholder verbatim.
    out.push_str(
        "<!-- corrections-section: rendered from corrections table; empty on first publish -->\n",
    );
    out.push_str("_No corrections have been recorded._\n\n");

    out.push_str("## Bibliography\n\n");
    if paper.bibliography.is_empty() {
        out.push_str("_No bibliography extracted._\n\n");
    } else {
        for (i, c) in paper.bibliography.iter().enumerate() {
            out.push_str(&format!("{}. {}", i + 1, c.raw));
            if let Some(d) = &c.doi {
                out.push_str(&format!(" doi:[{d}](https://doi.org/{d})"));
            }
            if let Some(a) = &c.arxiv_id {
                out.push_str(&format!(" arXiv:[{a}](https://arxiv.org/abs/{a})"));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    // Disclaimer is suppressed in the rendered review — see
    // crates/render/src/lib.rs::PUBLIC_DISCLAIMER. Reviews link to the web
    // app's /legal page if a reader wants the full text.
    out
}

fn recommendation_label(r: Recommendation) -> &'static str {
    match r {
        Recommendation::Accept => "Accept",
        Recommendation::MinorRevision => "Minor revision",
        Recommendation::MajorRevision => "Major revision",
        Recommendation::Reject => "Reject",
    }
}
