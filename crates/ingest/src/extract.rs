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

/// Pull a list of [`Citation`]s out of the text. We split on blank lines / `[n]`
/// markers inside the `References`/`Bibliography` section.
pub fn extract_bibliography(text: &str) -> Vec<Citation> {
    let Some(m) = BIB_HEADING_RE.find(text) else {
        return Vec::new();
    };
    let bib = &text[m.end()..];

    // Split on lines beginning with [n] or blank lines.
    let entry_split = Regex::new(r"(?m)^\s*(?:\[\d+\]|\(\d+\)|\d+\.)\s+|\n\s*\n").unwrap();
    let mut citations = Vec::new();
    for part in entry_split.split(bib) {
        let raw = part.trim();
        if raw.len() < 12 {
            continue;
        }
        let doi = DOI_RE.find(raw).map(|m| {
            m.as_str()
                .trim_end_matches(|c: char| ",.;)".contains(c))
                .to_string()
        });
        let arxiv_id = ARXIV_RE.find(raw).map(|m| m.as_str().to_string());
        citations.push(Citation {
            raw: raw.to_string(),
            doi,
            arxiv_id,
            title: None,
        });
    }
    citations
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
