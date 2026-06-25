//! Markdown renderer. Mirrors the HTML structure but in plain Markdown so
//! the artifact remains portable.

use grokrxiv_schemas::{
    MetaReview, PaperExtract, Recommendation, RevisionTarget, RevisionTargetKind,
    RevisionTargetStatus,
};

use crate::AgentRecord;

/// Render the public Markdown review.
pub fn render_markdown(meta: &MetaReview, paper: &PaperExtract, agents: &[AgentRecord]) -> String {
    let mut out = String::with_capacity(4096);
    let paper = crate::display::display_paper(paper);
    let meta = crate::display::display_meta(meta);

    // Prominent leading disclaimer as a blockquote.
    // Disclaimer suppressed — see crates/render/src/lib.rs::PUBLIC_DISCLAIMER.

    out.push_str(&format!("# {}\n\n", paper.title));
    let source_label = crate::paper_source_label(&paper.arxiv_id);
    if let Some(source_url) = crate::paper_source_url(&paper.arxiv_id) {
        out.push_str(&format!(
            "GrokRxiv review of [{source_label}]({source_url})"
        ));
    } else {
        out.push_str(&format!("GrokRxiv review of `{source_label}`"));
    }
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

    if !meta.revision_targets.is_empty() {
        out.push_str("## Revision Targets\n\n");
        for target in &meta.revision_targets {
            out.push_str(&revision_target_markdown(target));
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## Open Questions\n\n");
    for q in &meta.questions {
        out.push_str(&format!("- {q}\n"));
    }
    out.push('\n');

    out.push_str("## Per-Agent Reviews\n\n");
    for agent in agents {
        out.push_str(&format!(
            "### {} (`{}`) — status: `{}`\n\n",
            crate::role_slug(&agent.role),
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

fn revision_target_markdown(target: &RevisionTarget) -> String {
    let mut lines = vec![format!(
        "- {} **{}**{}",
        revision_target_checkbox(target.status),
        revision_target_heading(target),
        revision_target_status_suffix(target.status)
    )];
    lines.push(format!(
        "  - Location: {}",
        revision_target_location(target)
    ));
    if let Some(evidence) = target
        .evidence
        .as_deref()
        .filter(|s| !s.trim().is_empty() && s.trim() != target.required_update.trim())
    {
        lines.push(format!("  - Evidence: {evidence}"));
    }
    lines.push(format!("  - Required change: {}", target.required_update));
    if !target.verification_check.trim().is_empty() {
        lines.push(format!("  - Verification: {}", target.verification_check));
    }
    lines.join("\n")
}

fn revision_target_heading(target: &RevisionTarget) -> String {
    let locator = target.locator.as_deref().unwrap_or_default();
    match target.target_kind {
        RevisionTargetKind::Data => {
            if locator.contains("data availability") || locator.contains("restricted") {
                "Data availability and restricted inputs".to_string()
            } else {
                format!(
                    "Data target: {}",
                    short_text(non_empty(locator).unwrap_or(&target.required_update), 80)
                )
            }
        }
        RevisionTargetKind::Code => {
            if locator.contains("compute") {
                "Compute reproducibility".to_string()
            } else if locator.contains("configuration") {
                "Experiment configuration".to_string()
            } else if locator.contains("evaluation") {
                "Evaluation pipeline".to_string()
            } else if locator.contains("entrypoints") {
                "Code release and entrypoints".to_string()
            } else if locator.contains("SAC hyperparameters") {
                "SAC hyperparameters and reward scaling".to_string()
            } else if !locator.is_empty() {
                format!("Code/reproducibility target: {}", short_text(locator, 80))
            } else {
                "Code/reproducibility artifacts".to_string()
            }
        }
        RevisionTargetKind::Bibliography => format!(
            "Bibliography: {}",
            short_text(non_empty(locator).unwrap_or(&target.required_update), 96)
        ),
        RevisionTargetKind::PaperTex | RevisionTargetKind::PaperPdf => format!(
            "Manuscript: {}",
            short_text(non_empty(locator).unwrap_or(&target.required_update), 96)
        ),
        RevisionTargetKind::ReviewText => "Review text correction".to_string(),
        RevisionTargetKind::Unknown => format!(
            "Revision target: {}",
            short_text(&target.required_update, 96)
        ),
    }
}

fn revision_target_location(target: &RevisionTarget) -> String {
    let source_path = target
        .source_path
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let locator = target.locator.as_deref().filter(|s| !s.trim().is_empty());
    match (source_path, locator) {
        (Some(path), Some(locator)) => format!("`{path}` at `{locator}`"),
        (Some(path), None) => format!("`{path}`"),
        (None, Some(locator)) if target.target_kind == RevisionTargetKind::Data => {
            format!("data/reproducibility artifacts: `{locator}`")
        }
        (None, Some(locator)) if target.target_kind == RevisionTargetKind::Code => {
            format!("code/reproducibility artifacts: `{locator}`")
        }
        (None, Some(locator)) if target.target_kind == RevisionTargetKind::Bibliography => {
            format!("bibliography entry: `{}`", short_text(locator, 120))
        }
        (None, Some(locator)) => format!("`{locator}`"),
        (None, None) if target.target_kind == RevisionTargetKind::Data => {
            "data/reproducibility artifacts".to_string()
        }
        (None, None) if target.target_kind == RevisionTargetKind::Code => {
            "code/reproducibility artifacts".to_string()
        }
        (None, None) if target.target_kind == RevisionTargetKind::Bibliography => {
            "bibliography".to_string()
        }
        _ => "review artifact".to_string(),
    }
}

fn revision_target_checkbox(status: RevisionTargetStatus) -> &'static str {
    match status {
        RevisionTargetStatus::Addressed => "[x]",
        _ => "[ ]",
    }
}

fn revision_target_status_suffix(status: RevisionTargetStatus) -> String {
    match status {
        RevisionTargetStatus::Open => String::new(),
        other => format!(" _({})_", revision_target_status(other)),
    }
}

fn non_empty(text: &str) -> Option<&str> {
    (!text.trim().is_empty()).then_some(text)
}

fn short_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn revision_target_status(status: RevisionTargetStatus) -> &'static str {
    match status {
        RevisionTargetStatus::Open => "open",
        RevisionTargetStatus::Addressed => "addressed",
        RevisionTargetStatus::StillOpen => "still_open",
        RevisionTargetStatus::Superseded => "superseded",
        RevisionTargetStatus::Unknown => "unknown",
    }
}
