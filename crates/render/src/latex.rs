//! LaTeX renderer. Emits an `article`-class document with a `\bibliography{}`
//! stub. The public disclaimer is suppressed in the rendered document — it
//! lives only on the web app's `/legal` page.

use grokrxiv_schemas::{MetaReview, PaperExtract, Recommendation};

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
    bullet_section(&mut out, "Open Questions", &meta.questions);

    out.push_str("\\section*{Per-Agent Reviews}\n");
    for agent in agents {
        out.push_str(&format!(
            "\\subsection*{{{} (\\texttt{{{}}})}}\n",
            crate::role_slug(agent.role),
            latex_escape(&agent.model)
        ));
        out.push_str("\\begin{verbatim}\n");
        out.push_str(
            &serde_json::to_string_pretty(&agent.output).unwrap_or_else(|_| "{}".to_string()),
        );
        out.push_str("\n\\end{verbatim}\n\n");
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

fn recommendation_label(r: Recommendation) -> &'static str {
    match r {
        Recommendation::Accept => "Accept",
        Recommendation::MinorRevision => "Minor revision",
        Recommendation::MajorRevision => "Major revision",
        Recommendation::Reject => "Reject",
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
            _ => out.push(ch),
        }
    }
    out
}
