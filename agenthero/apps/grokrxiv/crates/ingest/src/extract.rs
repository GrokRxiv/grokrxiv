//! PDF and bibliography extraction heuristics.
//!
//! Good-enough-for-MVP regex-based section/bibliography splitter. The PDF
//! parser is `pdf-extract`, a pure-Rust crate with no native dependencies.

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::types::{Citation, Section};

/// Text plus lightweight cleanup metrics from PDF normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPdfText {
    pub text: String,
    pub joined_hyphenated_breaks: usize,
    pub removed_repeated_lines: usize,
    pub removed_page_markers: usize,
}

/// Convert a PDF byte buffer to a UTF-8 text blob.
pub fn pdf_to_text(pdf: &[u8]) -> Result<String> {
    pdf_extract::extract_text_from_mem(pdf).context("pdf_extract::extract_text_from_mem")
}

/// Normalize text produced by PDF extractors before downstream heuristics or
/// sample reviews see it.
///
/// PDF text extraction often preserves page headers, footer page numbers,
/// ligatures, and line-wrapped hyphenation. Those artifacts make the sample
/// upload path look worse than the full arXiv source path and waste LLM
/// context. This function stays deterministic and conservative: it removes
/// only repeated short header/footer lines and obvious page markers, and it
/// joins only alphabetic hyphenated line breaks.
pub fn normalize_pdf_text(raw: &str) -> NormalizedPdfText {
    let mut text = raw
        .replace('\u{000c}', "\n")
        .replace('\u{00a0}', " ")
        .replace('\r', "\n")
        .replace('\u{fb00}', "ff")
        .replace('\u{fb01}', "fi")
        .replace('\u{fb02}', "fl")
        .replace('\u{fb03}', "ffi")
        .replace('\u{fb04}', "ffl");

    let joined_hyphenated_breaks = HYPHENATED_LINEBREAK_RE.find_iter(&text).count();
    text = HYPHENATED_LINEBREAK_RE
        .replace_all(&text, "$word$next")
        .into_owned();

    let lines: Vec<String> = text
        .lines()
        .map(|line| INLINE_SPACE_RE.replace_all(line.trim(), " ").into_owned())
        .collect();

    let mut repeated_counts = std::collections::HashMap::<String, usize>::new();
    for line in &lines {
        if is_repeatable_header_footer(line) {
            *repeated_counts.entry(line.to_string()).or_default() += 1;
        }
    }

    let mut removed_repeated_lines = 0;
    let mut removed_page_markers = 0;
    let mut kept = Vec::with_capacity(lines.len());
    for line in lines {
        if PAGE_MARKER_RE.is_match(&line) {
            removed_page_markers += 1;
            continue;
        }
        if repeated_counts.get(&line).copied().unwrap_or(0) >= 3 {
            removed_repeated_lines += 1;
            continue;
        }
        kept.push(line);
    }

    let text = BLANK_LINES_RE
        .replace_all(kept.join("\n").trim(), "\n\n")
        .into_owned();

    NormalizedPdfText {
        text,
        joined_hyphenated_breaks,
        removed_repeated_lines,
        removed_page_markers,
    }
}

/// Infer a manuscript title from the opening normalized PDF text.
///
/// This is intentionally conservative. It only considers lines before major
/// paper body boundaries and returns `None` unless the candidate looks more
/// like a wrapped title than front-matter noise.
pub(crate) fn infer_pdf_title(text: &str) -> Option<String> {
    const MAX_TITLE_LEN: usize = 180;

    let mut best: Option<String> = None;
    let mut current: Vec<String> = Vec::new();

    for raw_line in text.lines().take(80) {
        let line = INLINE_SPACE_RE.replace_all(raw_line.trim(), " ");
        let line = line.trim();
        if line.is_empty() {
            finish_title_candidate(&mut current, &mut best, MAX_TITLE_LEN);
            continue;
        }
        if is_title_boundary(line) {
            break;
        }
        if is_pdf_title_noise(line, current.iter().map(|s| word_count(s)).sum()) {
            finish_title_candidate(&mut current, &mut best, MAX_TITLE_LEN);
            continue;
        }
        if line.len() > MAX_TITLE_LEN {
            finish_title_candidate(&mut current, &mut best, MAX_TITLE_LEN);
            continue;
        }
        current.push(line.to_string());
        if current.join(" ").len() > MAX_TITLE_LEN {
            current.pop();
            finish_title_candidate(&mut current, &mut best, MAX_TITLE_LEN);
        }
    }
    finish_title_candidate(&mut current, &mut best, MAX_TITLE_LEN);

    best
}

static HEADING_RE: Lazy<Regex> = Lazy::new(|| {
    // `^(\d+(\.\d+)*\s+)?[A-Z][A-Za-z ]{2,}$`
    Regex::new(r"^(?P<num>\d+(?:\.\d+)*)?\s*(?P<title>[A-Z][A-Za-z ]{2,})$").unwrap()
});
static HYPHENATED_LINEBREAK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)(?P<word>[A-Za-z]{2,})-\s*\n\s*(?P<next>[a-z][A-Za-z]{1,})").unwrap()
});
static INLINE_SPACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]+").unwrap());
static BLANK_LINES_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());
static PAGE_MARKER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:page\s+)?\d+\s*(?:of\s+\d+)?$").unwrap());
static URL_OR_DOI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(https?://|www\.|doi:|\b10\.\d{4,9}/)").unwrap());
static ARXIV_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\barxiv:\s*\d{4}\.\d{4,5}|\b\d{4}\.\d{4,5}v\d+\b").unwrap());
static DATE_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        ^(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s+\d{1,2},?\s+\d{4}$
        |^\d{1,2}\s+(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s+\d{4}$
        |^\d{4}-\d{2}-\d{2}$
        ",
    )
    .unwrap()
});
static EMAIL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").unwrap());
static AFFILIATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(university|institute|department|school of|college|laboratory|lab|centre|center|faculty)\b",
    )
    .unwrap()
});

fn is_repeatable_header_footer(line: &str) -> bool {
    if line.len() < 4 || line.len() > 96 {
        return false;
    }
    if line.ends_with('.') {
        return false;
    }
    let word_count = line.split_whitespace().count();
    word_count <= 10 && line.chars().any(|c| c.is_ascii_alphabetic())
}

fn finish_title_candidate(
    current: &mut Vec<String>,
    best: &mut Option<String>,
    max_title_len: usize,
) {
    if current.is_empty() {
        return;
    }
    let candidate = current.join(" ");
    current.clear();
    if !is_confident_pdf_title(&candidate, max_title_len) {
        return;
    }
    if best
        .as_ref()
        .map(|existing| title_score(&candidate) > title_score(existing) + 30)
        .unwrap_or(true)
    {
        *best = Some(candidate);
    }
}

fn is_confident_pdf_title(candidate: &str, max_title_len: usize) -> bool {
    let words = word_count(candidate);
    words >= 4
        && candidate.len() <= max_title_len
        && candidate.chars().any(|c| c.is_ascii_lowercase())
        && !candidate.ends_with('.')
        && !URL_OR_DOI_RE.is_match(candidate)
        && !ARXIV_LINE_RE.is_match(candidate)
}

fn title_score(title: &str) -> usize {
    let words = word_count(title);
    let length_bonus = title.len().min(120) / 12;
    words * 3 + length_bonus
}

fn is_title_boundary(line: &str) -> bool {
    matches!(
        line.trim_matches(|c: char| c == ':' || c.is_whitespace())
            .to_ascii_lowercase()
            .as_str(),
        "abstract" | "introduction" | "1 introduction" | "references" | "bibliography"
    )
}

fn is_pdf_title_noise(line: &str, current_words: usize) -> bool {
    if PAGE_MARKER_RE.is_match(line)
        || URL_OR_DOI_RE.is_match(line)
        || ARXIV_LINE_RE.is_match(line)
        || DATE_LINE_RE.is_match(line)
        || EMAIL_RE.is_match(line)
        || AFFILIATION_RE.is_match(line)
    {
        return true;
    }

    let lower = line.to_ascii_lowercase();
    if lower.starts_with("grokrxiv:") || (line.starts_with('[') && line.ends_with(']')) {
        return true;
    }
    if matches!(
        lower.as_str(),
        "abstract" | "contents" | "table of contents" | "preprint" | "draft"
    ) {
        return true;
    }

    let words = word_count(line);
    if words == 0 {
        return true;
    }
    if line.len() < 8 {
        return true;
    }
    if words == 1 {
        return current_words == 0 || is_all_caps_text(line);
    }
    if current_words >= 7 && looks_like_post_title_front_matter(line) {
        return true;
    }
    if words <= 4 && is_all_caps_text(line) {
        return true;
    }
    if current_words >= 7 && looks_like_author_name_line(line) {
        return true;
    }

    false
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn is_all_caps_text(line: &str) -> bool {
    let letters: String = line.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    !letters.is_empty() && letters.chars().all(|c| c.is_ascii_uppercase())
}

fn looks_like_author_name_line(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 2 || words.len() > 4 {
        return false;
    }
    words.iter().all(|word| {
        let trimmed = word.trim_matches(|c: char| c == ',' || c == ';' || c == '*');
        let mut chars = trimmed.chars();
        matches!(chars.next(), Some(first) if first.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_lowercase() || c == '-' || c == '\'')
    })
}

fn looks_like_post_title_front_matter(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("research")
        || lower.contains(" series")
        || lower.contains("paper ")
        || lower.starts_with("paper ")
}

/// Split a text blob into [`Section`]s using a heading-line heuristic.
///
/// Heading line shape: `^(\d+(\.\d+)*\s+)?[A-Z][A-Za-z ]{2,}$` — i.e. an
/// optional dotted-number prefix followed by a Title-Case phrase on its own
/// line. The numbering, when present, is prepended to the heading string so
/// downstream renderers can keep it visible without a separate field.
pub fn split_sections(text: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if let Some(cap) = HEADING_RE.captures(line) {
            let title = cap
                .name("title")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            // Filter out lines that almost certainly aren't headings.
            let looks_like_heading = title.len() <= 80
                && title.split_whitespace().count() <= 10
                && !title.ends_with('.');
            if looks_like_heading {
                if let Some(prev) = current.take() {
                    sections.push(prev);
                }
                let heading = match cap.name("num") {
                    Some(n) => format!("{} {}", n.as_str(), title),
                    None => title,
                };
                current = Some(Section {
                    heading,
                    body_markdown: String::new(),
                });
                continue;
            }
        }
        if let Some(sec) = current.as_mut() {
            if !sec.body_markdown.is_empty() {
                sec.body_markdown.push('\n');
            }
            sec.body_markdown.push_str(line);
        }
    }
    if let Some(prev) = current.take() {
        sections.push(prev);
    }
    sections
}

static DOI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b10\.\d{4,9}/[-._;()/:A-Z0-9]+").unwrap());
static ARXIV_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{4}\.\d{4,5}\b").unwrap());
static BIB_HEADING_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?im)^\s*(References|Bibliography)\s*$").unwrap());
static EXPLICIT_BIB_ENTRY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:\[\d{1,3}\]|\(\d{1,3}\)|\d{1,3}\.)\s+").unwrap());
static BIB_YEAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(?:19|20)\d{2}[a-z]?\b|\?\?\?\?").unwrap());

/// Pull a list of [`Citation`]s out of the text. We split on blank lines / `[n]`
/// markers inside the `References`/`Bibliography` section.
pub fn extract_bibliography(text: &str) -> Vec<Citation> {
    let Some(m) = BIB_HEADING_RE.find(text) else {
        return Vec::new();
    };
    let bib = &text[m.end()..];

    let parts = split_bibliography_entries(bib);
    let mut citations = Vec::new();
    for raw in parts {
        if raw.len() < 12 {
            continue;
        }
        let doi = DOI_RE.find(&raw).map(|m| {
            m.as_str()
                .trim_end_matches(|c: char| ",.;)".contains(c))
                .to_string()
        });
        let arxiv_id = ARXIV_RE.find(&raw).map(|m| m.as_str().to_string());
        citations.push(Citation {
            raw,
            doi,
            arxiv_id,
            title: None,
        });
    }
    citations
}

fn split_bibliography_entries(bib: &str) -> Vec<String> {
    let coarse_entries: Vec<String> = if EXPLICIT_BIB_ENTRY_RE.is_match(bib) {
        Regex::new(r"(?m)^\s*(?:\[\d{1,3}\]|\(\d{1,3}\)|\d{1,3}\.)\s+|\n\s*\n")
            .unwrap()
            .split(bib)
            .map(clean_bib_entry)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![bib.to_string()]
    };

    let mut entries = Vec::new();
    for entry in coarse_entries {
        let author_year_entries = split_author_year_bibliography_entries(&entry);
        if author_year_entries.len() > 1 {
            entries.extend(author_year_entries);
        } else {
            let entry = clean_bib_entry(&entry);
            if !entry.is_empty() {
                entries.push(entry);
            }
        }
    }
    entries
}

fn split_author_year_bibliography_entries(bib: &str) -> Vec<String> {
    let lines: Vec<&str> = bib
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('#'))
        .collect();
    let mut entries = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let lookahead = lines
            .iter()
            .skip(idx + 1)
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let next = if lookahead.is_empty() {
            None
        } else {
            Some(lookahead.as_str())
        };
        if !current.is_empty()
            && !current
                .last()
                .is_some_and(|prev| line_continues_author_list(prev))
            && looks_like_author_year_reference_start(line, next)
        {
            let entry = clean_bib_entry(&current.join(" "));
            if !entry.is_empty() {
                entries.push(entry);
            }
            current.clear();
        }
        current.push(line);
    }
    let entry = clean_bib_entry(&current.join(" "));
    if !entry.is_empty() {
        entries.push(entry);
    }
    entries
}

fn line_continues_author_list(line: &str) -> bool {
    line.trim()
        .trim_end_matches(|c: char| c == ',' || c == ';')
        .to_ascii_lowercase()
        .ends_with(" and")
}

fn clean_bib_entry(raw: &str) -> String {
    INLINE_SPACE_RE.replace_all(raw.trim(), " ").into_owned()
}

fn looks_like_author_year_reference_start(line: &str, next: Option<&str>) -> bool {
    if line.starts_with('#') {
        return false;
    }
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("and ") {
        return false;
    }
    let line_has_year = BIB_YEAR_RE.is_match(trimmed);
    if !line_has_year && !trimmed.contains([',', ';']) && !lower.contains(" and ") {
        return false;
    }
    let combined = if line_has_year {
        trimmed.to_string()
    } else {
        match next {
            Some(next) => format!("{trimmed} {}", next.trim()),
            None => trimmed.to_string(),
        }
    };
    let Some(year_match) = BIB_YEAR_RE.find(&combined) else {
        return false;
    };
    let author_prefix = combined[..year_match.start()].trim();
    looks_like_author_prefix(author_prefix)
}

fn looks_like_author_prefix(prefix: &str) -> bool {
    if prefix.is_empty() || prefix.len() > 240 {
        return false;
    }
    let lower = prefix.to_ascii_lowercase();
    if lower.starts_with("and ") || lower.contains(':') || lower.contains(" in ") {
        return false;
    }
    let word_count = prefix.split_whitespace().count();
    if word_count > 32 {
        return false;
    }
    let first_name = prefix
        .split([';', ','])
        .next()
        .unwrap_or(prefix)
        .trim()
        .trim_end_matches('.');
    let alpha_count = first_name.chars().filter(|c| c.is_alphabetic()).count();
    let starts_upper = first_name
        .chars()
        .find(|c| c.is_alphabetic())
        .map(|c| c.is_uppercase())
        .unwrap_or(false);
    if alpha_count < 2 || !starts_upper {
        return false;
    }
    if prefix.contains(',') && !prefix.contains(';') && !has_author_initial_after_comma(prefix) {
        return false;
    }
    prefix.contains(',') || prefix.contains(';') || lower.contains(" and ") || word_count <= 4
}

fn has_author_initial_after_comma(prefix: &str) -> bool {
    prefix
        .split_once(',')
        .map(|(_, after)| after.trim_start().chars().take(2).collect::<String>())
        .map(|head| {
            let mut chars = head.chars();
            matches!(chars.next(), Some(c) if c.is_uppercase()) && matches!(chars.next(), Some('.'))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_multiline_pdf_title_from_opening_text() {
        let text = "\
1
Robust Local Review
Abstractions for Scientific PDF Ingestion
Jane Doe
University of Example

Abstract
We study local review ingestion.
";

        assert_eq!(
            infer_pdf_title(text).as_deref(),
            Some("Robust Local Review Abstractions for Scientific PDF Ingestion")
        );
    }

    #[test]
    fn infers_pdf_title_before_abstract_boundary_and_skips_noise() {
        let text = "\
arXiv:2605.12345v1 [cs.DL] 18 May 2026
https://example.org/paper
10.1234/example.paper
May 18, 2026
18 May 2026
CONTENTS
Reliable Extraction from Local Manuscripts
Without Metadata
Alice Author alice@example.edu
Department of Computer Science, Example University

Abstract
This line must not be selected as title.
";

        assert_eq!(
            infer_pdf_title(text).as_deref(),
            Some("Reliable Extraction from Local Manuscripts Without Metadata")
        );
    }

    #[test]
    fn infers_pdf_title_with_one_word_wrapped_tail() {
        let text = "\
30 Apr 2026
[ math.CT ]
GrokRxiv:2026.04.mathematical-formalisms

Law I — Mathematical Formalisms:
Categorical Foundations for Matter–Information
Correspondence
MagnetonIO Research
Emergent Spacetime Dynamics Series, Paper 1 of 4
30 April 2026
Abstract
We formulate the categorical grammar.
";

        assert_eq!(
            infer_pdf_title(text).as_deref(),
            Some(
                "Law I — Mathematical Formalisms: Categorical Foundations for Matter–Information Correspondence"
            )
        );
    }

    #[test]
    fn returns_none_when_pdf_opening_has_no_confident_title() {
        let text = "\
1
Abstract
We study a system.

Introduction
The rest of the manuscript starts here.
";

        assert_eq!(infer_pdf_title(text), None);
    }

    #[test]
    fn normalizes_pdf_ligatures_headers_pages_and_hyphenation() {
        let raw = "\
GrokRxiv Draft
1
ABSTRACT
We study ef\u{fb01}cient con-
vergence for models.
GrokRxiv Draft
2
INTRODUCTION
The method is useful.
GrokRxiv Draft
3
REFERENCES
[1] Doe. Test. doi:10.1234/example.
";
        let normalized = normalize_pdf_text(raw);
        assert!(normalized.text.contains("efficient convergence"));
        assert!(!normalized.text.contains("GrokRxiv Draft"));
        assert!(!normalized.text.lines().any(|line| line == "1"));
        assert_eq!(normalized.joined_hyphenated_breaks, 1);
        assert_eq!(normalized.removed_repeated_lines, 3);
        assert_eq!(normalized.removed_page_markers, 3);
    }

    #[test]
    fn heading_split_basic() {
        let text = "Introduction\nWe study things.\nThis is text.\n\nMethods\nWe did things.";
        let sections = split_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].heading, "Introduction");
        assert!(sections[0].body_markdown.contains("We study things."));
        assert_eq!(sections[1].heading, "Methods");
    }

    #[test]
    fn bibliography_doi_extraction() {
        let text = "References\n[1] Smith J. A paper. doi:10.1234/abc.def.123 (2023).\n[2] Doe A. arXiv:2401.12345 (2024).";
        let cites = extract_bibliography(text);
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0].doi.as_deref(), Some("10.1234/abc.def.123"));
        assert_eq!(cites[1].arxiv_id.as_deref(), Some("2401.12345"));
    }

    #[test]
    fn bibliography_splits_author_year_pdf_references_without_blank_lines() {
        let text = "\
Body.

References
Abbas, T.; Rathore, S. A.; Turki, A.; Khan, S.; Alghushairy,
O.; and Daud, A. 2025. Enhancing Software Engineering
With AI: Innovations, Challenges, and Future Directions.
IET Software, 2025(1): 5691460.
Alam, A. 2023. Harnessing the power of AI to create intelligent tutoring systems for enhanced classroom experience
and improved learning outcomes. In Intelligent communication technologies and virtual mobile networks, 571-591.
Springer.
Allen, T. J. 1977. Managing the flow of technology: technology transfer and the dissemination of technological information within the R and D organization.
Bhat, A.; Aubin Le Quere, M.; Naaman, M.; and Jakesch, M.
2026. Reactive Writers: How Co-Writing with AI Changes
How We Engage with Ideas. In Proceedings of CHI, 1-21.
Ehsan, U.; Passi, S.; Saha, K.; McNutt, T.; Riedl, M. O.; and
Alcorn, S. 2026. From Future of Work to Future of Workers.
Meske, C.; Hermanns, T.; Von der Weiden, E.; Loser, K.-U.;
and Berger, T. 2025. Vibe coding as a reconfiguration of
intent mediation in software development. IEEE Access, 13: 213242-213259.
Time. 2023. How to end the unfairness of invisible work.
Wells, J. E.; and MacAulay, D. ???? What Invisible Work Looks Like in the 21st Century.
Zhang, X.; Subramonyam, H.; Sarkar, A.; Drosos, I.; Wang,
Z.; Lee, K.; Pimenova, V.; Chen, X.; and Lukoff, K.
2026. Generative Design and Vibe Coding: Rethinking The
Design-Development Divide for UI Prototyping.
";
        let cites = extract_bibliography(text);
        let raws: Vec<&str> = cites.iter().map(|c| c.raw.as_str()).collect();
        assert_eq!(raws.len(), 9, "{raws:#?}");
        assert!(raws[0].starts_with("Abbas, T."));
        assert!(raws[3].starts_with("Bhat, A."));
        assert!(raws[4].starts_with("Ehsan, U."));
        assert!(raws[4].contains("and Alcorn, S. 2026"));
        assert!(raws[5].starts_with("Meske, C."));
        assert!(raws[5].contains("and Berger, T. 2025"));
        assert!(raws[8].starts_with("Zhang, X."));
        assert!(raws[8].contains("Z.; Lee, K."));
    }
}
