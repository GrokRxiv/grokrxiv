use std::collections::HashMap;

use crate::agents::config::{self, AgentConfig, BibliographyMode};

#[cfg(feature = "grokrxiv-ingest")]
pub(super) struct ReviewPromptFacts<'a> {
    pub(super) moderator_notes: Option<&'a str>,
    pub(super) reproducibility: Option<&'a crate::agents::review::facts::ReproducibilityFacts>,
    pub(super) novelty: Option<&'a crate::agents::review::facts::NoveltyFacts>,
    pub(super) technical: Option<&'a crate::agents::review::facts::TechnicalCorrectnessFacts>,
}

pub(super) fn render_system_prompt(
    role_id: &str,
    cfg: &AgentConfig,
    field: Option<&str>,
) -> String {
    let (system_template, _) = prompt_template_sections(cfg);
    let description = cfg.role.as_deref().unwrap_or(role_id);
    let mut vars = HashMap::new();
    vars.insert("role_id", role_id.to_string());
    vars.insert("role", description.to_string());
    vars.insert("field", field.unwrap_or("").to_string());
    let mut system = if system_template.trim().is_empty() {
        format!(
            "You are the `{role_id}` agent in a GrokRxiv DAG app. \
             {description} Respond with strict JSON conforming to the supplied schema. \
             No prose, no code fences, no commentary."
        )
    } else {
        render_template(&system_template, &vars)
    };
    for overlay in &cfg.system_overlays {
        if let Some(text) = system_overlay_text(overlay, field) {
            if !system.ends_with('\n') {
                system.push('\n');
            }
            system.push('\n');
            system.push_str(text);
        }
    }
    system
}

fn system_overlay_text(name: &str, field: Option<&str>) -> Option<&'static str> {
    let code_amenable = field.map(is_code_amenable_field).unwrap_or(false);
    match name {
        "proof_as_code_technical" if code_amenable => Some(
            "PROOF-AS-CODE AXIOM. The paper is in a code-amenable field. For every \
             load-bearing claim that could be supported by an executable artifact and \
             the paper does not ship that artifact: record the claim as unsupported, \
             severity at least major, and name a concrete file path for the missing \
             proof, simulation, benchmark, or evaluation script.",
        ),
        "proof_as_code_reproducibility" if code_amenable => Some(
            "PROOF-AS-CODE AXIOM. The paper is in a code-amenable field. Theory papers \
             are not exempt from reproducibility analysis: formal verification or \
             numerical reproduction counts as reproducibility. Missing proof/code \
             artifacts for load-bearing claims are reproducibility concerns.",
        ),
        "meta_recommendation_gate" if code_amenable => Some(
            "RECOMMENDATION GATE. If specialist outputs flagged a missing proof-as-code \
             artifact at major or critical severity, default recommendation to \
             major_revision. If the missing artifact blocks a headline claim, recommend \
             reject. Verified facts in verifier_notes are authoritative provenance.",
        ),
        _ => None,
    }
}

pub(super) fn is_code_amenable_field(field: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "cs.", "math.", "hep-", "gr-qc", "astro-ph", "cond-mat", "nlin", "quant-ph", "nucl-",
        "stat.",
    ];
    PREFIXES.iter().any(|p| field.starts_with(p))
}

#[cfg(feature = "grokrxiv-ingest")]
const DEFAULT_CITATION_PROMPT_MAX_BIB_ENTRIES: usize = 32;

#[cfg(feature = "grokrxiv-ingest")]
fn citation_prompt_max_bib_entries() -> usize {
    std::env::var("GROKRXIV_CITATION_PROMPT_MAX_BIB_ENTRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CITATION_PROMPT_MAX_BIB_ENTRIES)
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_section(heading: &str, body: &str) -> String {
    format!("## {heading}\n\n{body}\n\n")
}

#[cfg(feature = "grokrxiv-ingest")]
fn truncate_60_40(s: &str, budget: usize) -> String {
    let total = s.chars().count();
    if total <= budget {
        return s.to_string();
    }
    let marker = "\n\n[…truncated…]\n\n";
    let marker_len = marker.chars().count();
    let usable = budget.saturating_sub(marker_len);
    let head_n = (usable * 60) / 100;
    let tail_n = usable.saturating_sub(head_n);
    let head: String = s.chars().take(head_n).collect();
    let tail: String = s.chars().skip(total - tail_n).collect();
    format!("{head}{marker}{tail}")
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_section_block(sections: &[grokrxiv_schemas::Section], budget: usize) -> String {
    if budget == 0 || sections.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut consumed: usize = 0;

    for s in sections {
        let rendered = render_section(&s.heading, &s.body_markdown);
        let rendered_chars = rendered.chars().count();
        if consumed == 0 && rendered_chars > budget {
            let truncated = truncate_60_40(&rendered, budget);
            consumed = truncated.chars().count();
            out.push_str(&truncated);
            continue;
        }
        if consumed + rendered_chars <= budget {
            out.push_str(&rendered);
            consumed += rendered_chars;
        } else {
            skipped.push(s.heading.as_str());
        }
    }

    if !skipped.is_empty() {
        let headings = skipped.join("; ");
        out.push_str(&format!(
            "[…remaining sections truncated; headings: {headings}]\n"
        ));
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_bibliography(bibliography: &[grokrxiv_schemas::Citation]) -> String {
    render_bibliography_limited(bibliography, None)
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_bibliography_limited(
    bibliography: &[grokrxiv_schemas::Citation],
    max_entries: Option<usize>,
) -> String {
    if bibliography.is_empty() {
        return String::new();
    }
    let total = bibliography.len();
    let shown = max_entries.map(|max| max.min(total)).unwrap_or(total);
    let mut out = if shown < total {
        format!("Bibliography ({total} entries; showing {shown}):\n")
    } else {
        format!("Bibliography ({total} entries):\n")
    };
    for (i, c) in bibliography.iter().take(shown).enumerate() {
        let key = i + 1;
        let raw = c.raw.replace('\n', " ").trim().to_string();
        let mut parts = Vec::new();
        if !raw.is_empty() {
            parts.push(raw.clone());
        }
        if let Some(title) = c.title.as_deref().filter(|s| !s.trim().is_empty()) {
            if !raw.contains(title) {
                parts.push(format!("title: {}", title.trim()));
            }
        }
        if let Some(doi) = c.doi.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(format!("doi: {}", doi.trim()));
        }
        if let Some(arxiv_id) = c.arxiv_id.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(format!("arxiv: {}", arxiv_id.trim()));
        }
        if parts.is_empty() {
            parts.push("unresolved bibliography entry".to_string());
        }
        out.push_str(&format!("[{key}] {}\n", parts.join(" | ")));
    }
    if shown < total {
        let omitted = total - shown;
        out.push_str(&format!(
            "[…{omitted} additional bibliography entries omitted from this LLM prompt; \
             verifier and render artifacts preserve the full bibliography.]\n"
        ));
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_citation_contexts(sections: &[grokrxiv_schemas::Section], budget: usize) -> String {
    if budget == 0 || sections.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for section in sections {
        for sentence in citation_sentences(&section.body_markdown) {
            let line = format!("- {}: {}\n", section.heading, sentence);
            let next_len = out.chars().count() + line.chars().count();
            if next_len > budget {
                if out.is_empty() {
                    out.push_str(&truncate_60_40(&line, budget));
                }
                return out;
            }
            out.push_str(&line);
        }
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn citation_sentences(body: &str) -> Vec<String> {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in normalized.chars() {
        current.push(ch);
        if matches!(ch, '.' | '?' | '!') {
            push_citation_sentence(&mut sentences, &current);
            current.clear();
        }
    }
    push_citation_sentence(&mut sentences, &current);
    sentences
}

#[cfg(feature = "grokrxiv-ingest")]
fn push_citation_sentence(sentences: &mut Vec<String>, sentence: &str) {
    let trimmed = sentence.trim();
    if trimmed.is_empty() {
        return;
    }
    if trimmed.contains("[@") || trimmed.contains('@') || trimmed.contains("\\cite") {
        sentences.push(truncate_60_40(trimmed, 1_200));
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn render_agent_user_prompt(
    role_id: &str,
    cfg: &AgentConfig,
    extract: &grokrxiv_schemas::PaperExtract,
    facts: ReviewPromptFacts<'_>,
) -> String {
    if cfg.prompt_context.meta_input {
        return String::new();
    }
    let (_, user_template) = prompt_template_sections(cfg);
    let template = if user_template.trim().is_empty() {
        "Title: {{title}}\n\nAbstract:\n{{abstract}}\n\nSections:\n{{sections}}\n\nBibliography:\n{{bibliography}}\n\n{{fact_blocks}}"
            .to_string()
    } else {
        user_template
    };

    let heading_index = extract
        .sections
        .iter()
        .take(40)
        .map(|s| format!("- {}", s.heading))
        .collect::<Vec<_>>()
        .join("\n");
    let sections = render_section_block(
        &extract.sections,
        cfg.prompt_context.body_budget_chars.unwrap_or(0),
    );
    let bibliography = match cfg.prompt_context.bibliography {
        BibliographyMode::None => String::new(),
        BibliographyMode::Full => render_bibliography(&extract.bibliography),
        BibliographyMode::Limited => render_bibliography_limited(
            &extract.bibliography,
            Some(
                cfg.prompt_context
                    .max_bibliography_entries
                    .unwrap_or_else(citation_prompt_max_bib_entries),
            ),
        ),
    };
    let citation_contexts = render_citation_contexts(
        &extract.sections,
        cfg.prompt_context
            .citation_context_budget_chars
            .unwrap_or_default(),
    );
    let fact_blocks = render_configured_fact_blocks(cfg, &facts);
    let moderator_notes = facts
        .moderator_notes
        .filter(|s| !s.trim().is_empty())
        .map(|notes| {
            format!(
                "Moderator notes from a prior request-changes round. Treat these as authoritative priorities:\n\n{}",
                notes.trim()
            )
        })
        .unwrap_or_default();

    let mut vars = HashMap::new();
    vars.insert("role_id", role_id.to_string());
    vars.insert("role", cfg.role.as_deref().unwrap_or(role_id).to_string());
    vars.insert("field", extract.field.clone().unwrap_or_default());
    vars.insert("title", extract.title.clone());
    vars.insert("abstract", extract.abstract_.clone());
    vars.insert("sections", sections);
    vars.insert("heading_index", heading_index);
    vars.insert("bibliography", bibliography);
    vars.insert("citation_contexts", citation_contexts);
    vars.insert("fact_blocks", fact_blocks);
    vars.insert("moderator_notes", moderator_notes);
    render_template(&template, &vars)
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_configured_fact_blocks(cfg: &AgentConfig, facts: &ReviewPromptFacts<'_>) -> String {
    let mut out = String::new();
    for block in &cfg.prompt_context.fact_blocks {
        match block.as_str() {
            "reproducibility_availability" => {
                if let Some(facts) = facts.reproducibility {
                    out.push_str(&render_reproducibility_fact_block(facts));
                }
            }
            "novelty_prior_art" => {
                if let Some(facts) = facts.novelty {
                    out.push_str(&render_novelty_fact_block(facts));
                }
            }
            "technical_structure" => {
                if let Some(facts) = facts.technical {
                    out.push_str(&render_technical_fact_block(facts));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_reproducibility_fact_block(
    facts: &crate::agents::review::facts::ReproducibilityFacts,
) -> String {
    if facts.urls_checked.is_empty() && facts.github_repos.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Verified availability facts (deterministically retrieved; treat as authoritative):\n\n",
    );
    if !facts.urls_checked.is_empty() {
        out.push_str("URLs checked:\n");
        for u in &facts.urls_checked {
            let status_str = u
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "network_error".to_string());
            let kind = match u.kind {
                crate::agents::review::facts::UrlKind::Code => "code",
                crate::agents::review::facts::UrlKind::Dataset => "dataset",
                crate::agents::review::facts::UrlKind::Other => "other",
            };
            out.push_str(&format!(
                "- [{kind}] {url} -> {state} (status={status_str})\n",
                url = u.url,
                state = if u.reachable {
                    "REACHABLE"
                } else {
                    "UNREACHABLE"
                },
            ));
        }
        out.push('\n');
    }
    if !facts.github_repos.is_empty() {
        out.push_str("GitHub repositories:\n");
        for r in &facts.github_repos {
            if !r.exists {
                out.push_str(&format!(
                    "- {}/{}: NOT FOUND (404 or private without token)\n",
                    r.owner, r.repo
                ));
                continue;
            }
            let mut tags: Vec<String> = Vec::new();
            if let Some(p) = &r.pushed_at {
                tags.push(format!("last_pushed={p}"));
            }
            if let Some(s) = r.stargazers_count {
                tags.push(format!("stars={s}"));
            }
            if let Some(l) = &r.license_spdx {
                tags.push(format!("license={l}"));
            }
            if matches!(r.archived, Some(true)) {
                tags.push("ARCHIVED".to_string());
            }
            out.push_str(&format!(
                "- {}/{}: exists; {}\n",
                r.owner,
                r.repo,
                if tags.is_empty() {
                    "no metadata".to_string()
                } else {
                    tags.join(", ")
                }
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "Rules:\n\
         - A code_url from the paper is resolved only if its entry above is REACHABLE.\n\
         - ARCHIVED repositories are maintenance concerns.\n\
         - UNREACHABLE code/dataset URLs are major reproducibility concerns.\n\n",
    );
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_novelty_fact_block(facts: &crate::agents::review::facts::NoveltyFacts) -> String {
    if facts.related_papers.is_empty() {
        if facts.retrieval_error.is_empty() {
            return String::new();
        }
        return format!(
            "Prior-art retrieval failed ({}); fall back to memory but lower confidence.\n\n",
            facts.retrieval_error
        );
    }
    let mut out = String::from(
        "Verified prior-art candidates (retrieved by metadata similarity; judge novelty against these):\n\n",
    );
    for (i, p) in facts.related_papers.iter().enumerate().take(20) {
        let year = p
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "n.d.".to_string());
        let author = p.primary_author.as_deref().unwrap_or("unknown");
        let snippet = p
            .abstract_snippet
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no abstract)");
        out.push_str(&format!(
            "{:>2}. [{year}] {author} - {title}\n    {snippet}\n",
            i + 1,
            title = p.title,
        ));
        if let Some(arxiv) = &p.arxiv_id {
            out.push_str(&format!("    arXiv:{arxiv}\n"));
        }
        if let Some(doi) = &p.doi {
            out.push_str(&format!("    doi:{doi}\n"));
        }
    }
    out.push_str(
        "\nRules:\n\
         - Papers listed here are real, retrievable neighbors.\n\
         - If a listed paper conflicts with the manuscript's novelty claim, lower novelty_score and add missing_prior_art.\n\n",
    );
    out
}

#[cfg(feature = "grokrxiv-ingest")]
fn render_technical_fact_block(
    facts: &crate::agents::review::facts::TechnicalCorrectnessFacts,
) -> String {
    if facts.tables.is_empty()
        && facts.equation_labels.is_empty()
        && facts.complexity_mentions.is_empty()
    {
        return String::new();
    }
    let mut out = String::from(
        "Verified structural facts about the paper (use these to cross-check claims):\n\n",
    );
    if !facts.tables.is_empty() {
        out.push_str("Tables found:\n");
        for t in facts.tables.iter().take(20) {
            out.push_str(&format!(
                "- [{section}] {rows} rows; header: {header}\n",
                section = t.section,
                rows = t.row_count,
                header = t.header_row.chars().take(160).collect::<String>(),
            ));
        }
        out.push('\n');
    }
    if !facts.equation_labels.is_empty() {
        out.push_str("Equation labels found:\n");
        for e in facts.equation_labels.iter().take(20) {
            out.push_str(&format!("- [{}] {}\n", e.section, e.label));
        }
        out.push('\n');
    }
    if !facts.complexity_mentions.is_empty() {
        out.push_str("Complexity notations found:\n");
        for c in facts.complexity_mentions.iter().take(20) {
            out.push_str(&format!("- [{}] {}\n", c.section, c.notation));
        }
        out.push('\n');
    }
    out
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn render_meta_synthesis_prompt(
    cfg: &AgentConfig,
    meta_input: &serde_json::Value,
) -> String {
    let (_, user_template) = prompt_template_sections(cfg);
    let pretty = serde_json::to_string_pretty(meta_input).unwrap_or_else(|_| "{}".into());
    if user_template.trim().is_empty() {
        return format!("Specialist reviews:\n{pretty}");
    }
    let mut vars = HashMap::new();
    vars.insert("specialists", pretty.clone());
    vars.insert("meta_input", pretty);
    render_template(&user_template, &vars)
}

/// Resolve the debug-prompt directory from the `GROKRXIV_DEBUG_PROMPT_DIR`
/// env var, set by the CLI's `--debug-prompt` flag. When the var is unset
/// (or empty) this returns `None` and the supervisor skips the dump.
#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn debug_prompt_root() -> Option<std::path::PathBuf> {
    let raw = std::env::var("GROKRXIV_DEBUG_PROMPT_DIR").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(raw))
}

/// Best-effort dump of one role's rendered prompt under
/// `<root>/<arxiv_id>/<role>.md`. Silently does nothing on any I/O failure —
/// `--debug-prompt` is observational and must never crash a review.
#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn dump_debug_prompt(
    root: &std::path::Path,
    arxiv_id: &str,
    role_id: &str,
    prompt: &str,
) {
    let safe_id: String = arxiv_id
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c => c,
        })
        .collect();
    let dir = root.join(safe_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = dir.join(format!("{}.md", role_id.replace('.', "_")));
    let _ = std::fs::write(&file, prompt);
}

fn prompt_template_sections(cfg: &AgentConfig) -> (String, String) {
    let Some(path) = cfg.prompt_template.as_deref() else {
        return (String::new(), String::new());
    };
    let path = config::resolve_declared_runtime_path(path);
    let Ok(text) = std::fs::read_to_string(path) else {
        return (String::new(), String::new());
    };
    split_prompt_template(&text)
}

fn split_prompt_template(text: &str) -> (String, String) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        None,
        System,
        User,
    }
    let mut current = Section::None;
    let mut system = String::new();
    let mut user = String::new();
    for line in text.lines() {
        match line.trim() {
            "# System" => {
                current = Section::System;
                continue;
            }
            "# User" => {
                current = Section::User;
                continue;
            }
            _ => {}
        }
        match current {
            Section::System => {
                system.push_str(line);
                system.push('\n');
            }
            Section::User => {
                user.push_str(line);
                user.push('\n');
            }
            Section::None => {}
        }
    }
    if system.is_empty() && user.is_empty() {
        user = text.to_string();
    }
    (system.trim().to_string(), user.trim().to_string())
}

fn render_template(template: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}
