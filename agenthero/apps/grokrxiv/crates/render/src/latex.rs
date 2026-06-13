//! LaTeX renderer. Emits an `article`-class document with a `\bibliography{}`
//! stub. The public disclaimer is suppressed in the rendered document — it
//! lives only on the web app's `/legal` page.

use grokrxiv_schemas::{
    MetaReview, PaperExtract, Recommendation, RevisionTarget, RevisionTargetKind,
    RevisionTargetStatus,
};

use crate::AgentRecord;

/// Render the public LaTeX review.
pub fn render_latex(meta: &MetaReview, paper: &PaperExtract, agents: &[AgentRecord]) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(
        r#"\documentclass[11pt]{article}
\usepackage[margin=1in]{geometry}
\usepackage[utf8]{inputenc}
\usepackage{hyperref}
\usepackage{amsmath,amssymb}
\usepackage{fancyhdr}
\usepackage{enumitem}
\pagestyle{fancy}
\fancyhf{}
\fancyfoot[C]{\thepage}
"#,
    );

    out.push_str(&format!("\\title{{{}}}\n", latex_escape(&paper.title),));

    if !paper.authors.is_empty() {
        let authors: Vec<String> = paper
            .authors
            .iter()
            .map(|a| latex_escape(&a.name))
            .collect();
        out.push_str(&format!("\\author{{{}}}\n", authors.join(" \\and ")));
    }

    out.push_str("\\date{}\n");
    out.push_str("\\begin{document}\n");
    out.push_str("\\maketitle\n\n");

    let source_label = crate::paper_source_label(&paper.arxiv_id);
    if let Some(source_url) = crate::paper_source_url(&paper.arxiv_id) {
        out.push_str(&format!(
            "\\noindent\\textbf{{Source:}} \\href{{{}}}{{{}}}",
            latex_escape(&source_url),
            latex_escape(&source_label)
        ));
    } else {
        out.push_str(&format!(
            "\\noindent\\textbf{{Source:}} \\texttt{{{}}}",
            latex_escape(&source_label)
        ));
    }
    if let Some(field) = &paper.field {
        out.push_str(&format!(
            " \\quad \\textbf{{Field:}} {}",
            latex_escape(field)
        ));
    }
    out.push_str("\\\\\n\n");

    out.push_str("\\section*{TL;DR}\n");
    out.push_str(&latex_escape(&meta.summary));
    out.push_str("\n\n");
    out.push_str(&format!(
        "\\textbf{{Recommendation:}} {} \\quad \\textbf{{Confidence:}} {:.0}\\%\\\\\n\n",
        recommendation_label(meta.recommendation),
        meta.confidence.clamp(0.0, 1.0) * 100.0
    ));

    bullet_section(&mut out, "Strengths", &meta.strengths);
    bullet_section(&mut out, "Weaknesses", &meta.weaknesses);
    let revision_targets = revision_target_lines(&meta.revision_targets);
    if !revision_targets.is_empty() {
        bullet_section(&mut out, "Revision Targets", &revision_targets);
    }
    bullet_section(&mut out, "Open Questions", &meta.questions);

    out.push_str("\\section*{Per-Agent Reviews}\n");
    for agent in agents {
        out.push_str(&format!(
            "\\subsection*{{{} (\\texttt{{{}}})}}\n",
            latex_escape(&crate::role_slug(&agent.role)),
            latex_escape(&agent.model)
        ));
        let json = serde_json::to_string_pretty(&agent.output).unwrap_or_else(|_| "{}".to_string());
        latex_code_block(&mut out, &json);
    }

    out.push_str("\\section*{Corrections}\n");
    out.push_str("% corrections rendered from corrections table; empty on first publish\n");
    out.push_str("No corrections have been recorded.\n\n");

    out.push_str("\\section*{Bibliography}\n");
    if paper.bibliography.is_empty() {
        out.push_str("No bibliography extracted.\n\n");
    } else {
        out.push_str("\\begin{enumerate}[leftmargin=*]\n");
        for c in &paper.bibliography {
            out.push_str("  \\item ");
            out.push_str(&latex_escape(&c.raw));
            if let Some(d) = &c.doi {
                out.push_str(&format!(
                    " \\href{{https://doi.org/{0}}}{{doi:{0}}}",
                    latex_escape(d)
                ));
            }
            if let Some(a) = &c.arxiv_id {
                out.push_str(&format!(
                    " \\href{{https://arxiv.org/abs/{0}}}{{arXiv:{0}}}",
                    latex_escape(a)
                ));
            }
            out.push('\n');
        }
        out.push_str("\\end{enumerate}\n\n");
    }

    // Stub for downstream tooling — the orchestrator can swap in a real .bib file.
    out.push_str("% \\bibliographystyle{plain}\n");
    out.push_str("% \\bibliography{references}\n");

    out.push_str("\\end{document}\n");
    out
}

fn bullet_section(out: &mut String, title: &str, items: &[String]) {
    out.push_str(&format!("\\section*{{{}}}\n", title));
    if items.is_empty() {
        out.push_str("\\textit{None.}\n\n");
        return;
    }
    out.push_str("\\begin{itemize}[leftmargin=*]\n");
    for item in items {
        out.push_str("  \\item ");
        out.push_str(&latex_escape(item));
        out.push('\n');
    }
    out.push_str("\\end{itemize}\n\n");
}

fn latex_code_block(out: &mut String, text: &str) {
    out.push_str("\\begin{quote}\\small\\ttfamily\\raggedright\n");
    for line in text.lines() {
        out.push_str(&latex_escape(line));
        out.push_str("\\\\\n");
    }
    out.push_str("\\end{quote}\n\n");
}

fn recommendation_label(r: Recommendation) -> &'static str {
    match r {
        Recommendation::Accept => "Accept",
        Recommendation::MinorRevision => "Minor revision",
        Recommendation::MajorRevision => "Major revision",
        Recommendation::Reject => "Reject",
    }
}

fn revision_target_lines(targets: &[RevisionTarget]) -> Vec<String> {
    targets
        .iter()
        .map(|target| {
            let mut line = format!(
                "[{}] {}",
                revision_target_status(target.status),
                revision_target_kind(target.target_kind)
            );
            if let Some(path) = target
                .source_path
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                line.push_str(&format!(" `{path}`"));
            }
            if let Some(locator) = target.locator.as_deref().filter(|s| !s.trim().is_empty()) {
                line.push_str(&format!(" at `{locator}`"));
            }
            line.push_str(&format!(": {}", target.required_update));
            if !target.verification_check.trim().is_empty() {
                line.push_str(&format!(" Check: {}", target.verification_check));
            }
            line
        })
        .collect()
}

fn revision_target_kind(kind: RevisionTargetKind) -> &'static str {
    match kind {
        RevisionTargetKind::PaperTex => "paper_tex",
        RevisionTargetKind::PaperPdf => "paper_pdf",
        RevisionTargetKind::Code => "code",
        RevisionTargetKind::Data => "data",
        RevisionTargetKind::Bibliography => "bibliography",
        RevisionTargetKind::ReviewText => "review_text",
        RevisionTargetKind::Unknown => "unknown",
    }
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

fn latex_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '$' => out.push_str("\\$"),
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '^' => out.push_str("\\^{}"),
            '~' => out.push_str("\\~{}"),
            '\u{03b1}' => out.push_str("\\ensuremath{\\alpha}"),
            '\u{03b2}' => out.push_str("\\ensuremath{\\beta}"),
            '\u{03b3}' => out.push_str("\\ensuremath{\\gamma}"),
            '\u{03b4}' => out.push_str("\\ensuremath{\\delta}"),
            '\u{03b5}' => out.push_str("\\ensuremath{\\epsilon}"),
            '\u{03b6}' => out.push_str("\\ensuremath{\\zeta}"),
            '\u{03b7}' => out.push_str("\\ensuremath{\\eta}"),
            '\u{03b8}' => out.push_str("\\ensuremath{\\theta}"),
            '\u{03b9}' => out.push_str("\\ensuremath{\\iota}"),
            '\u{03ba}' => out.push_str("\\ensuremath{\\kappa}"),
            '\u{03bb}' => out.push_str("\\ensuremath{\\lambda}"),
            '\u{03bc}' => out.push_str("\\ensuremath{\\mu}"),
            '\u{03bd}' => out.push_str("\\ensuremath{\\nu}"),
            '\u{03be}' => out.push_str("\\ensuremath{\\xi}"),
            '\u{03c0}' => out.push_str("\\ensuremath{\\pi}"),
            '\u{03c1}' => out.push_str("\\ensuremath{\\rho}"),
            '\u{03c2}' => out.push_str("\\ensuremath{\\varsigma}"),
            '\u{03c3}' => out.push_str("\\ensuremath{\\sigma}"),
            '\u{03c4}' => out.push_str("\\ensuremath{\\tau}"),
            '\u{03c5}' => out.push_str("\\ensuremath{\\upsilon}"),
            '\u{03c6}' => out.push_str("\\ensuremath{\\phi}"),
            '\u{03c7}' => out.push_str("\\ensuremath{\\chi}"),
            '\u{03c8}' => out.push_str("\\ensuremath{\\psi}"),
            '\u{03c9}' => out.push_str("\\ensuremath{\\omega}"),
            '\u{03d5}' => out.push_str("\\ensuremath{\\varphi}"),
            '\u{0393}' => out.push_str("\\ensuremath{\\Gamma}"),
            '\u{0394}' => out.push_str("\\ensuremath{\\Delta}"),
            '\u{0398}' => out.push_str("\\ensuremath{\\Theta}"),
            '\u{039b}' => out.push_str("\\ensuremath{\\Lambda}"),
            '\u{039e}' => out.push_str("\\ensuremath{\\Xi}"),
            '\u{03a0}' => out.push_str("\\ensuremath{\\Pi}"),
            '\u{03a3}' => out.push_str("\\ensuremath{\\Sigma}"),
            '\u{03a5}' => out.push_str("\\ensuremath{\\Upsilon}"),
            '\u{03a6}' => out.push_str("\\ensuremath{\\Phi}"),
            '\u{03a8}' => out.push_str("\\ensuremath{\\Psi}"),
            '\u{03a9}' => out.push_str("\\ensuremath{\\Omega}"),
            '\u{2212}' => out.push_str("\\ensuremath{-}"),
            '\u{223c}' => out.push_str("\\ensuremath{\\sim}"),
            '\u{2248}' => out.push_str("\\ensuremath{\\approx}"),
            '\u{2243}' => out.push_str("\\ensuremath{\\simeq}"),
            '\u{2260}' => out.push_str("\\ensuremath{\\ne}"),
            '\u{2264}' => out.push_str("\\ensuremath{\\le}"),
            '\u{2265}' => out.push_str("\\ensuremath{\\ge}"),
            '\u{00b1}' => out.push_str("\\ensuremath{\\pm}"),
            '\u{00b7}' => out.push_str("\\ensuremath{\\cdot}"),
            '\u{00bd}' => out.push_str("\\ensuremath{\\frac{1}{2}}"),
            '\u{00d7}' => out.push_str("\\ensuremath{\\times}"),
            '\u{2297}' => out.push_str("\\ensuremath{\\otimes}"),
            '\u{2295}' => out.push_str("\\ensuremath{\\oplus}"),
            '\u{2218}' => out.push_str("\\ensuremath{\\circ}"),
            '\u{2227}' => out.push_str("\\ensuremath{\\wedge}"),
            '\u{2228}' => out.push_str("\\ensuremath{\\vee}"),
            '\u{2208}' => out.push_str("\\ensuremath{\\in}"),
            '\u{2209}' => out.push_str("\\ensuremath{\\notin}"),
            '\u{2282}' => out.push_str("\\ensuremath{\\subset}"),
            '\u{2286}' => out.push_str("\\ensuremath{\\subseteq}"),
            '\u{221e}' => out.push_str("\\ensuremath{\\infty}"),
            '\u{2202}' => out.push_str("\\ensuremath{\\partial}"),
            '\u{2207}' => out.push_str("\\ensuremath{\\nabla}"),
            '\u{2192}' => out.push_str("\\ensuremath{\\to}"),
            '\u{2190}' => out.push_str("\\ensuremath{\\leftarrow}"),
            '\u{21a6}' => out.push_str("\\ensuremath{\\mapsto}"),
            '\u{21d2}' => out.push_str("\\ensuremath{\\Rightarrow}"),
            '\u{00b2}' => out.push_str("\\textsuperscript{2}"),
            '\u{00b3}' => out.push_str("\\textsuperscript{3}"),
            '\u{00b9}' => out.push_str("\\textsuperscript{1}"),
            '\u{2070}' => out.push_str("\\textsuperscript{0}"),
            '\u{2074}' => out.push_str("\\textsuperscript{4}"),
            '\u{2075}' => out.push_str("\\textsuperscript{5}"),
            '\u{2076}' => out.push_str("\\textsuperscript{6}"),
            '\u{2077}' => out.push_str("\\textsuperscript{7}"),
            '\u{2078}' => out.push_str("\\textsuperscript{8}"),
            '\u{2079}' => out.push_str("\\textsuperscript{9}"),
            '\u{0300}' => out.push_str("\\`{}"),
            '\u{0301}' => out.push_str("\\'{}"),
            '\u{0302}' => out.push_str("\\^{}"),
            '\u{0303}' => out.push_str("\\~{}"),
            '\u{0304}' => out.push_str("\\={}"),
            '\u{030c}' => out.push_str("\\v{}"),
            '\u{0307}' => out.push_str("\\.{}"),
            '\u{0308}' => out.push_str("\\\"{}"),
            '\u{2013}' => out.push_str("--"),
            '\u{2014}' => out.push_str("---"),
            _ => out.push(ch),
        }
    }
    out
}
