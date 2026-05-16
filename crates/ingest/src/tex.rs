//! LaTeX source bundle extraction.
//!
//! Fetches `https://arxiv.org/e-print/<id>` (a `tar.gz` for almost every
//! paper, or a single `.tex` file for very simple submissions), pulls the
//! "main" TeX file out, and runs a pragmatic regex parser over it to build a
//! [`PaperExtract`]. Math typesetting is preserved as raw LaTeX in section
//! bodies — review agents see `$x^2$` rather than the lossy PDF transcription
//! of "x2".
//!
//! This is intentionally NOT a full LaTeX parser. We extract:
//! - `\title{...}` and `\author{...}` (fallback to the Atom metadata)
//! - `\begin{abstract}...\end{abstract}`
//! - `\section{...}` / `\subsection{...}` headings with their body text
//! - `\bibitem{...}` / `\cite{...}` entries
//!
//! Comments (`%...`) are stripped before parsing. `\input{...}` and
//! `\include{...}` are resolved one level deep against the unpacked bundle.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use flate2::read::GzDecoder;
use regex::Regex;
use tar::Archive;

use crate::types::{Citation, Section};

const ARXIV_SOURCE: &str = "https://arxiv.org/e-print/";

/// Result of pulling the source bundle and parsing it.
pub struct TexExtract {
    /// Title parsed from `\title{...}`; empty if not found.
    pub title: String,
    /// Abstract text from `\begin{abstract}...\end{abstract}`; empty if not found.
    pub abstract_text: String,
    /// Body sections in document order.
    pub sections: Vec<Section>,
    /// Bibliography entries.
    pub bibliography: Vec<Citation>,
}

/// Build the canonical source URL for an arXiv id.
pub fn source_url(arxiv_id: &str) -> String {
    format!("{ARXIV_SOURCE}{arxiv_id}")
}

/// Parse an arXiv source bundle (tar.gz, plain tar, gzip, or single .tex)
/// into a [`TexExtract`]. Returns an error if the bundle is unreadable or
/// contains no `.tex` files.
pub fn parse_bundle(bytes: &Bytes) -> Result<TexExtract> {
    let files = unpack(bytes)?;
    if files.is_empty() {
        return Err(anyhow!("source bundle contained no .tex files"));
    }
    let main = pick_main(&files);
    let raw = files
        .get(&main)
        .cloned()
        .unwrap_or_else(|| files.values().next().cloned().unwrap_or_default());
    let resolved = resolve_inputs(&raw, &files, 0);
    let cleaned = strip_comments(&resolved);
    Ok(parse_tex(&cleaned))
}

// ---------------------------------------------------------------------------
// Bundle unpacking
// ---------------------------------------------------------------------------

/// Decode the raw bytes returned by arXiv's `e-print` endpoint into a
/// `{filename → contents}` map of `.tex` files.
///
/// arXiv returns one of: gzipped tar, plain tar, gzipped single .tex, raw .tex.
/// We try each in order.
fn unpack(bytes: &Bytes) -> Result<HashMap<String, String>> {
    // Path 1: gzipped tar
    if let Ok(map) = try_targz(bytes) {
        if !map.is_empty() {
            return Ok(map);
        }
    }
    // Path 2: plain tar
    if let Ok(map) = try_tar(bytes) {
        if !map.is_empty() {
            return Ok(map);
        }
    }
    // Path 3: gzipped single .tex
    if let Ok(text) = try_gz_single(bytes) {
        let mut m = HashMap::new();
        m.insert("main.tex".to_string(), text);
        return Ok(m);
    }
    // Path 4: raw .tex (utf-8)
    if let Ok(s) = std::str::from_utf8(bytes) {
        if s.contains("\\documentclass") || s.contains("\\begin{document}") {
            let mut m = HashMap::new();
            m.insert("main.tex".to_string(), s.to_string());
            return Ok(m);
        }
    }
    Err(anyhow!("source bundle is not in a recognised format"))
}

fn try_targz(bytes: &Bytes) -> Result<HashMap<String, String>> {
    let gz = GzDecoder::new(bytes.as_ref());
    let mut archive = Archive::new(gz);
    extract_tex_files(&mut archive)
}

fn try_tar(bytes: &Bytes) -> Result<HashMap<String, String>> {
    let mut archive = Archive::new(bytes.as_ref());
    extract_tex_files(&mut archive)
}

fn try_gz_single(bytes: &Bytes) -> Result<String> {
    let mut gz = GzDecoder::new(bytes.as_ref());
    let mut buf = String::new();
    gz.read_to_string(&mut buf).context("gz decode")?;
    if buf.contains("\\documentclass") || buf.contains("\\begin{document}") {
        Ok(buf)
    } else {
        Err(anyhow!("gz'd payload is not a .tex file"))
    }
}

fn extract_tex_files<R: Read>(archive: &mut Archive<R>) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        let path = entry.path().context("entry path")?.to_path_buf();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        if !name.ends_with(".tex") {
            continue;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_ok() {
            out.insert(name, text);
        }
    }
    Ok(out)
}

/// Heuristic: prefer the file containing `\documentclass`, fall back to
/// `main.tex`/`paper.tex`/`<id>.tex`, then the largest `.tex` by length.
fn pick_main(files: &HashMap<String, String>) -> String {
    for (name, contents) in files {
        if contents.contains("\\documentclass") {
            return name.clone();
        }
    }
    for candidate in &["main.tex", "paper.tex", "ms.tex"] {
        if files.contains_key(*candidate) {
            return candidate.to_string();
        }
    }
    files
        .iter()
        .max_by_key(|(_, v)| v.len())
        .map(|(k, _)| k.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// \input / \include resolution (one level deep)
// ---------------------------------------------------------------------------

fn resolve_inputs(src: &str, files: &HashMap<String, String>, depth: usize) -> String {
    if depth >= 2 {
        return src.to_string();
    }
    let re = Regex::new(r"\\(?:input|include)\{([^}]+)\}").unwrap();
    re.replace_all(src, |caps: &regex::Captures| {
        let mut name = caps[1].to_string();
        if !name.ends_with(".tex") {
            name.push_str(".tex");
        }
        let basename = name.rsplit('/').next().unwrap_or(&name);
        if let Some(content) = files.get(basename) {
            resolve_inputs(content, files, depth + 1)
        } else {
            caps[0].to_string()
        }
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Comment stripping
// ---------------------------------------------------------------------------

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut prev = '\0';
        let mut idx = None;
        for (i, ch) in line.char_indices() {
            if ch == '%' && prev != '\\' {
                idx = Some(i);
                break;
            }
            prev = ch;
        }
        let cleaned = match idx {
            Some(i) => &line[..i],
            None => line,
        };
        out.push_str(cleaned);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse_tex(src: &str) -> TexExtract {
    let title = extract_title(src);
    let abstract_text = extract_abstract(src);
    let sections = extract_sections(src);
    let bibliography = extract_bibliography(src);
    TexExtract {
        title,
        abstract_text,
        sections,
        bibliography,
    }
}

fn extract_title(src: &str) -> String {
    // Match \title{...} with balanced braces (one level).
    let re = Regex::new(r"\\title\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}").unwrap();
    re.captures(src)
        .and_then(|c| c.get(1))
        .map(|m| sanitize_inline(m.as_str()))
        .unwrap_or_default()
}

fn extract_abstract(src: &str) -> String {
    let re = Regex::new(r"(?s)\\begin\{abstract\}(.*?)\\end\{abstract\}").unwrap();
    re.captures(src)
        .and_then(|c| c.get(1))
        .map(|m| sanitize_inline(m.as_str()))
        .unwrap_or_default()
}

/// Split the document into sections using `\section{...}` (top-level only).
/// Subsections are kept inline within the parent section's body.
fn extract_sections(src: &str) -> Vec<Section> {
    let re = Regex::new(r"(?m)\\(section|subsection)\*?\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}")
        .unwrap();
    let matches: Vec<(usize, usize, String, String)> = re
        .captures_iter(src)
        .map(|c| {
            let kind = c.get(1).unwrap().as_str().to_string();
            let heading = sanitize_inline(c.get(2).unwrap().as_str());
            let start = c.get(0).unwrap().start();
            let end = c.get(0).unwrap().end();
            (start, end, kind, heading)
        })
        .collect();

    let mut sections = Vec::new();
    for (i, (start, header_end, kind, heading)) in matches.iter().enumerate() {
        if kind != "section" {
            continue;
        }
        let body_end = matches
            .iter()
            .skip(i + 1)
            .find(|(_, _, k, _)| k == "section")
            .map(|(s, _, _, _)| *s)
            .unwrap_or(src.len());
        let _ = start;
        let body = src[*header_end..body_end].to_string();
        sections.push(Section {
            heading: heading.clone(),
            body_markdown: sanitize_body(&body),
        });
    }
    sections
}

fn extract_bibliography(src: &str) -> Vec<Citation> {
    // The Rust `regex` crate doesn't support lookahead, so we run a two-pass
    // extraction: collect bibitem start/end positions, then slice the body
    // text between them.
    let re = Regex::new(r"\\bibitem(?:\[[^\]]*\])?\s*\{([^}]+)\}").unwrap();
    let entry_ends: Vec<usize> = re.find_iter(src).map(|m| m.end()).collect();
    if entry_ends.is_empty() {
        return Vec::new();
    }
    let bib_end = src
        .find("\\end{thebibliography}")
        .unwrap_or(src.len());

    let mut out = Vec::new();
    for (i, &content_start) in entry_ends.iter().enumerate() {
        let content_end = entry_ends.get(i + 1).copied().unwrap_or(bib_end);
        if content_end <= content_start {
            continue;
        }
        let raw = src[content_start..content_end].trim();
        if raw.is_empty() {
            continue;
        }
        let raw_clean = sanitize_inline(raw);
        let (doi, arxiv_id) = sniff_identifiers(&raw_clean);
        out.push(Citation {
            raw: raw_clean,
            doi,
            arxiv_id,
            title: None,
        });
    }
    out
}

fn sniff_identifiers(text: &str) -> (Option<String>, Option<String>) {
    let doi = Regex::new(r"\b10\.\d{4,9}/[-._;()/:A-Za-z0-9]+")
        .unwrap()
        .find(text)
        .map(|m| m.as_str().trim_end_matches(&[',', '.', ';'][..]).to_string());
    let arxiv = Regex::new(r"\b(?:arXiv:)?(\d{4}\.\d{4,5})(?:v\d+)?")
        .unwrap()
        .captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    (doi, arxiv)
}

/// Light cleanup for inline strings (titles, headings, abstract chunks).
fn sanitize_inline(s: &str) -> String {
    let mut t = s.to_string();
    // Strip simple wrapper commands like \textbf{...}, \emph{...}, \mathrm{...}.
    let wrap = Regex::new(
        r"\\(?:textbf|textit|emph|underline|texttt|textrm|mathrm|mathit|mathbf|mathsf|mathcal|operatorname)\s*\{([^{}]*)\}",
    )
    .unwrap();
    for _ in 0..3 {
        t = wrap.replace_all(&t, "$1").to_string();
    }
    // Newline / tilde / extra space cleanup.
    t = t.replace('\n', " ").replace('~', " ");
    let multispace = Regex::new(r"\s+").unwrap();
    multispace.replace_all(&t, " ").trim().to_string()
}

/// Body sanitiser — keeps inline math intact, trims surrounding whitespace.
fn sanitize_body(s: &str) -> String {
    let trimmed = s.trim();
    let multispace = Regex::new(r"\n{3,}").unwrap();
    multispace.replace_all(trimmed, "\n\n").to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
\documentclass{article}
\title{A Test Paper on $E = mc^2$}
\author{Jane Doe}
\begin{document}
\begin{abstract}
We extend Fourier analysis to curved spaces and prove a Parseval identity.
\end{abstract}
\section{Introduction}
Fourier transforms are well-studied. We extend them.
% this is a comment, should be stripped
\section{Methods}
Let $f \in L^2(M)$ for a Riemannian manifold $M$.
\section{References}
\begin{thebibliography}{}
\bibitem{ax1} Smith, J. 2020. arXiv:2001.12345. 10.1103/PhysRevX.10.011007
\bibitem{ax2} Doe, J. 2021. Some title.
\end{thebibliography}
\end{document}
"#;

    #[test]
    fn parses_title_abstract_sections_bib() {
        let cleaned = strip_comments(SAMPLE);
        let extract = parse_tex(&cleaned);
        assert!(extract.title.contains("Test Paper"));
        assert!(extract.abstract_text.contains("Fourier"));
        assert_eq!(extract.sections.len(), 3);
        assert_eq!(extract.sections[0].heading, "Introduction");
        assert_eq!(extract.sections[1].heading, "Methods");
        assert_eq!(extract.bibliography.len(), 2);
        assert_eq!(
            extract.bibliography[0].arxiv_id.as_deref(),
            Some("2001.12345")
        );
        assert!(extract.bibliography[0].doi.is_some());
    }

    #[test]
    fn comments_are_stripped() {
        let out = strip_comments("foo % real comment\nbar \\% literal percent");
        // The unescaped "% real comment" is gone.
        assert!(!out.contains("real comment"));
        // The escaped "\\%" survives along with its trailing text.
        assert!(out.contains("\\% literal percent"));
    }

    #[test]
    fn picks_main_by_documentclass() {
        let mut files = HashMap::new();
        files.insert("appendix.tex".to_string(), "no class here".to_string());
        files.insert(
            "paper.tex".to_string(),
            "\\documentclass{article}".to_string(),
        );
        assert_eq!(pick_main(&files), "paper.tex");
    }

    #[test]
    fn picks_main_by_largest_when_no_documentclass() {
        let mut files = HashMap::new();
        files.insert("tiny.tex".to_string(), "x".to_string());
        files.insert("big.tex".to_string(), "x".repeat(100));
        assert_eq!(pick_main(&files), "big.tex");
    }

    #[test]
    fn resolves_one_level_input() {
        let mut files = HashMap::new();
        files.insert(
            "main.tex".to_string(),
            "\\input{intro}\n\\section{Body}\nText".to_string(),
        );
        files.insert(
            "intro.tex".to_string(),
            "\\section{Intro}\nIntro text".to_string(),
        );
        let resolved = resolve_inputs(files.get("main.tex").unwrap(), &files, 0);
        assert!(resolved.contains("Intro text"));
        assert!(resolved.contains("Body"));
    }

    #[test]
    fn sanitize_inline_strips_wrapper_commands() {
        assert_eq!(sanitize_inline("\\textbf{Hello} World"), "Hello World");
        assert_eq!(
            sanitize_inline("\\emph{italic} text \\mathrm{x}"),
            "italic text x"
        );
    }
}
