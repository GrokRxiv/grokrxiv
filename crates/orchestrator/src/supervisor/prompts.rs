pub(super) fn role_system_prompt(role: grokrxiv_schemas::AgentRole, field: Option<&str>) -> String {
    use grokrxiv_schemas::AgentRole;
    let task = match role {
        AgentRole::Summary => "summarize papers in plain language for a literate non-expert",
        AgentRole::TechnicalCorrectness => {
            "assess mathematical, logical, and empirical correctness claim-by-claim"
        }
        AgentRole::Novelty => "compare against prior work and judge novelty",
        AgentRole::Reproducibility => "judge whether the work can be reproduced from the paper",
        AgentRole::Citation => "verify cited references and surface missing ones",
        AgentRole::MetaReviewer => {
            "synthesize five specialist reviews into a single recommendation"
        }
    };
    let mut s = format!(
        "You are a careful, honest specialist peer reviewer. You {task}. \
         Respond with strict JSON conforming to the supplied schema. No prose, \
         no code fences, no commentary."
    );
    let amenable = field.map(is_code_amenable_field).unwrap_or(false);
    if amenable {
        match role {
            AgentRole::TechnicalCorrectness => {
                s.push_str(
                    "\n\nPROOF-AS-CODE AXIOM. The paper is in a code-amenable field \
                     (cs.*, math.*, hep-*, gr-qc, astro-ph, cond-mat, nlin, quant-ph, nucl-*). \
                     For every load-bearing claim that COULD be supported by an executable \
                     artifact — a formal proof in Coq/Lean/Agda/Isabelle, a simulation or \
                     numerical method as Python/Julia/Rust, a complexity argument as \
                     benchmarks, an ML claim as training/eval scripts — and the paper does \
                     NOT ship that artifact: record the claim with assessment 'unsupported' \
                     and severity at least 'major' (use 'critical' if it blocks a headline \
                     result), and write a concrete suggested_fix that names where the code \
                     should live, e.g. `src/proofs/Thm3.lean`, `experiments/figure3/run.py`, \
                     `benchmarks/complexity_test.rs`. Override the default 'be conservative' \
                     guidance for these cases — absence of executable verification IS evidence \
                     of weakness in this field.",
                );
            }
            AgentRole::Reproducibility => {
                s.push_str(
                    "\n\nPROOF-AS-CODE AXIOM. The paper is in a code-amenable field. \
                     Theory papers are NOT exempt from reproducibility analysis: formal \
                     verification or numerical reproduction of theoretical results counts \
                     as reproducibility, and a claimed theorem without a formal proof or \
                     numerical evidence IS a reproducibility gap. For every load-bearing \
                     theoretical or empirical claim that lacks a code/proof artifact, add a \
                     `concerns` entry with area='proof_as_code', a description naming the \
                     specific artifact that would close the gap (path included), and \
                     severity at least 'major' ('critical' if the headline result depends on it).",
                );
            }
            AgentRole::MetaReviewer => {
                s.push_str(
                    "\n\nRECOMMENDATION GATE. When technical_correctness OR reproducibility \
                     flagged a missing proof-as-code artifact at severity 'major' or 'critical', \
                     default `recommendation` to `major_revision`. If the missing artifact \
                     blocks a headline claim, recommend `reject`. Only allow `accept` or \
                     `minor_revision` when (a) code exists and was acknowledged by the \
                     specialists, or (b) the paper explicitly justifies the absence (e.g. \
                     existence proof in a field where Coq tooling does not yet cover the \
                     theory). When applying this gate, cite the specific specialist findings \
                     in `summary` and add the missing artifacts to `weaknesses`.\n\n\
                     VERIFIED-FACT WEIGHTING. Deterministic verifier facts are stored in \
                     each specialist row's `verifier_notes`, not in LLM-authored fields. \
                     For citation, `verifier_notes.citation.notes.entries[*].status` is \
                     authoritative: `resolved` and `unresolved` are definitive, \
                     `transient_unknown` means the external service failed and must not \
                     be treated as a fake citation, and `malformed` means the identifier \
                     shape was invalid. Reproducibility reachability and novelty retrieval \
                     facts are also verifier-side provenance. Do not convert unknown or \
                     verifier-only facts into LLM judgments.",
                );
            }
            _ => {}
        }
    }
    s
}

pub(super) fn is_code_amenable_field(field: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "cs.", "math.", "hep-", "gr-qc", "astro-ph", "cond-mat", "nlin", "quant-ph", "nucl-",
        "stat.",
    ];
    PREFIXES.iter().any(|p| field.starts_with(p))
}

/// Per-role character budget for the rendered section bodies. Reserved for
/// the body block only; title, abstract, heading index, and bibliography are
/// outside this budget. The budgets keep specialist prompts inside the target
/// model context window after schema overhead.
///
/// `MetaReviewer` is `0` because it only receives specialist outputs.
#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn body_budget_chars(role: grokrxiv_schemas::AgentRole) -> usize {
    use grokrxiv_schemas::AgentRole;
    match role {
        AgentRole::Summary => 48_000,
        AgentRole::TechnicalCorrectness => 240_000,
        AgentRole::Novelty => 120_000,
        AgentRole::Reproducibility => 80_000,
        AgentRole::Citation => 0,
        AgentRole::MetaReviewer => 0,
    }
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

/// Render a single section in its canonical `## {heading}\n\n{body}\n\n`
/// form. The trailing blank line keeps adjacent sections visually separated.
#[cfg(feature = "grokrxiv-ingest")]
fn render_section(heading: &str, body: &str) -> String {
    format!("## {heading}\n\n{body}\n\n")
}

/// Truncate `s` to roughly `budget` chars using the "first 60%, last 40%"
/// split. Char-based (not byte-based) so we never split a multi-byte codepoint.
/// If `s` already fits, returns it untouched.
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

/// Render the section body block within `budget` chars.
///
/// Behavior:
/// - Iterate sections in document order. For each:
///   - Render `## {heading}\n\n{body}\n\n`.
///   - If the rendered single section exceeds `budget` on its own AND nothing
///     has been emitted yet, truncate it with the 60/40 split and emit it as
///     the sole survivor.
///   - Otherwise, if it fits in remaining budget, append it.
///   - Otherwise, skip it and record the heading as truncated.
/// - If any sections are skipped, append a single
///   `[…remaining sections truncated; headings: a; b; c]` block.
///
/// `budget == 0` returns an empty string (used for `MetaReviewer`).
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

/// Render the bibliography block. Keys are synthesised 1-indexed
/// (`[1] …`, `[2] …`) since `Citation` doesn't carry a BibTeX key field; this
/// keeps the format stable across runs and is what the citation specialist
/// expects to cross-reference.
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
            "[…{omitted} additional bibliography entries omitted from citation LLM prompt; \
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
                    let truncated = truncate_60_40(&line, budget);
                    out.push_str(&truncated);
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
    if trimmed.contains("[@") || trimmed.contains("@") || trimmed.contains("\\cite") {
        sentences.push(truncate_60_40(trimmed, 1_200));
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn build_specialist_prompt(
    role: grokrxiv_schemas::AgentRole,
    extract: &grokrxiv_schemas::PaperExtract,
    moderator_notes: Option<&str>,
    reproducibility_facts: Option<&crate::agents::specialist_facts::ReproducibilityFacts>,
    novelty_facts: Option<&crate::agents::specialist_facts::NoveltyFacts>,
    tc_facts: Option<&crate::agents::specialist_facts::TechnicalCorrectnessFacts>,
) -> String {
    use grokrxiv_schemas::AgentRole;

    // MetaReviewer receives only the specialist-output bundle, not the paper body.
    if matches!(role, AgentRole::MetaReviewer) {
        return String::new();
    }

    let budget = body_budget_chars(role);
    let heading_index: String = extract
        .sections
        .iter()
        .take(40)
        .map(|s| format!("- {}", s.heading))
        .collect::<Vec<_>>()
        .join("\n");
    let body_block = render_section_block(&extract.sections, budget);
    let bib_block = if matches!(role, AgentRole::Citation) {
        render_bibliography_limited(
            &extract.bibliography,
            Some(citation_prompt_max_bib_entries()),
        )
    } else {
        render_bibliography(&extract.bibliography)
    };
    let citation_contexts = if matches!(role, AgentRole::Citation) {
        render_citation_contexts(&extract.sections, 24_000)
    } else {
        String::new()
    };

    let task = match role {
        AgentRole::Summary => {
            "Produce a plain-language summary of the paper. Populate the schema's \
             `plain_language_summary`, `key_contributions`, `tldr`, and (optionally) \
             `audience` fields."
        }
        AgentRole::TechnicalCorrectness => {
            "Walk through the paper's main claims and assess each. Populate the schema's \
             `claims` (with id, claim, assessment, severity, and optionally location, \
             evidence, suggested_fix), `overall_correctness`, and `confidence`."
        }
        AgentRole::Novelty => {
            "Compare this paper against the most relevant prior work and judge its \
             novelty. Populate `novelty_score`, `verdict`, `confidence`, and optionally \
             `related_work` and `missing_prior_art`."
        }
        AgentRole::Reproducibility => {
            "Evaluate reproducibility. Populate `code_availability`, `data_availability`, \
             `reproducibility_score`, `confidence`, and optionally `code_url`, `data_url`, \
             `environment`, `concerns`."
        }
        AgentRole::Citation => {
            "Focus on RELEVANCE and MISSING WORK — a separate deterministic \
             verifier (Crossref + arXiv batch lookups) handles existence and \
             DOI/URL resolution and writes its results to `verifier_notes`. \
             Your job: for each bibliography entry included below, set `relevance` \
             from the extracted in-text contexts (`high`/`medium`/`low`/`unrelated`), \
             write `explanation` describing where and why it's cited, and \
             leave `exists`/`resolved_doi`/`resolved_url` at their defaults \
             (`null`/`null`/`null`) since verifier provenance in \
             `verifier_notes` is authoritative. \
             If the bibliography block says entries were omitted, do not invent \
             entries for the omitted references; the verifier and render pipeline \
             preserve the full bibliography separately. \
             Populate `missing_references` with prior work you would expect \
             the paper to cite but doesn't, with reasons. Provide `summary` \
             and `confidence`."
        }
        AgentRole::MetaReviewer => unreachable!("MetaReviewer handled above"),
    };

    let field_line = match extract.field.as_deref() {
        Some(f) if !f.is_empty() => format!("Paper field: {f}\n\n"),
        _ => String::new(),
    };
    let mut out = format!(
        "{field_line}Paper title: {title}\n\nAbstract:\n{abstract_}\n\nSection headings:\n{heading_index}\n\n",
        title = extract.title,
        abstract_ = extract.abstract_,
    );
    if !body_block.is_empty() {
        out.push_str("Paper body:\n\n");
        out.push_str(&body_block);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    if !citation_contexts.is_empty() {
        out.push_str("Citation contexts:\n\n");
        out.push_str(&citation_contexts);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    if !bib_block.is_empty() {
        out.push_str(&bib_block);
        out.push('\n');
    }
    if let Some(notes) = moderator_notes.filter(|s| !s.trim().is_empty()) {
        out.push_str(
            "Moderator notes from a prior `request-changes` round — treat these as authoritative \
             priorities for this review pass:\n\n",
        );
        out.push_str(notes.trim());
        out.push_str("\n\n");
    }
    if let Some(facts) = reproducibility_facts {
        if !facts.urls_checked.is_empty() || !facts.github_repos.is_empty() {
            out.push_str(
                "Verified availability facts (deterministically retrieved — do NOT re-check, \
                 treat as authoritative):\n\n",
            );
            if !facts.urls_checked.is_empty() {
                out.push_str("URLs checked:\n");
                for u in &facts.urls_checked {
                    let status_str = u
                        .status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "network_error".to_string());
                    let kind = match u.kind {
                        crate::agents::specialist_facts::UrlKind::Code => "code",
                        crate::agents::specialist_facts::UrlKind::Dataset => "dataset",
                        crate::agents::specialist_facts::UrlKind::Other => "other",
                    };
                    out.push_str(&format!(
                        "- [{kind}] {url} → {state} (status={status_str})\n",
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
                 - A `code_url` from the paper is only resolved iff its entry above is REACHABLE.\n\
                 - A repository marked ARCHIVED implies the work is no longer maintained — \
                   surface as a `severity: minor` concern.\n\
                 - An UNREACHABLE code/dataset URL is a `severity: major` reproducibility concern; \
                   add a `concerns` entry naming the URL and the status code.\n\n",
            );
        }
    }
    if let Some(facts) = novelty_facts {
        if !facts.related_papers.is_empty() {
            out.push_str(
                "Verified prior-art candidates (retrieved by metadata similarity — judge novelty \
                 against these, do NOT rely on memory of pre-2024 literature):\n\n",
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
                    "{:>2}. [{year}] {author} — {title}\n    {snippet}\n",
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
                 - Each related paper above is a real, retrievable neighbor — treat its existence \
                   as ground truth. Do NOT claim a paper does not exist if it's listed here.\n\
                 - When the manuscript's novelty claim conflicts with a related paper, lower \
                   `novelty_score` and add a `missing_prior_art` entry citing the related paper.\n\n",
            );
        } else if !facts.retrieval_error.is_empty() {
            out.push_str(&format!(
                "Prior-art retrieval failed ({}); fall back to memory but flag the gap in confidence.\n\n",
                facts.retrieval_error,
            ));
        }
    }
    if let Some(facts) = tc_facts {
        if !facts.tables.is_empty()
            || !facts.equation_labels.is_empty()
            || !facts.complexity_mentions.is_empty()
        {
            out.push_str(
                "Verified structural facts about the paper (use these to cross-check claims \
                 against actual tables and equations; do NOT reason from memory of the body):\n\n",
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
            out.push_str(
                "Rules:\n\
                 - When a claim references a number that should appear in a table above, cite \
                   the table by header and verify the number against the source body block.\n\
                 - When the paper claims a complexity bound not listed above, flag it as \
                   `unsupported` unless the body explicitly derives it.\n\n",
            );
        }
    }
    out.push_str(&format!("Task: {task}"));
    out
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
    role: grokrxiv_schemas::AgentRole,
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
    let file = dir.join(format!("{}.md", super::role_slug(role)));
    let _ = std::fs::write(&file, prompt);
}

pub(super) fn build_meta_synthesis_prompt(meta_input: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(meta_input).unwrap_or_else(|_| "{}".into());
    // The meta input contract contains only the `specialists` key; the paper
    // extract is intentionally omitted because each specialist already used it.
    format!(
        "Below is a JSON object with one key, `specialists`, containing the five \
         specialist reviewers' outputs keyed by role slug:\n\
         - `summary` → {{tldr, plain_language_summary, key_contributions[], audience}}\n\
         - `technical_correctness` → {{claims[], overall_correctness, confidence}}\n\
         - `novelty` → {{verdict, novelty_score, related_work[], missing_prior_art[], confidence}}\n\
         - `reproducibility` → {{reproducibility_score, code_availability, code_url, \
            data_availability, data_url, environment, concerns[], confidence}}\n\
         - `citation` → {{entries[], missing_references[], summary, confidence}}\n\n\
         The paper extract itself is NOT included — each specialist already reasoned \
         over it. Treat the specialist outputs as your sole evidence.\n\n\
         {pretty}\n\n\
         Task: Synthesize these five specialist reviews into a single MetaReview JSON \
         object with fields summary, strengths, weaknesses, questions, recommendation \
         (one of accept|minor_revision|major_revision|reject), and confidence (0..1)."
    )
}
