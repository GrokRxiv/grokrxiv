//! LaTeX source bundle extraction (Pandoc subprocess pipeline with optional
//! LaTeXML semantic enrichment).
//!
//! Pipeline:
//! 1. Unpack the arXiv source bundle (tar.gz / tar / gzip / raw .tex) to a
//!    temp dir, preserving relative paths so `\input{chapters/intro}`
//!    resolves naturally.
//! 2. Pick the main `.tex` file by `\documentclass` + `\begin{document}`.
//! 3. Run `pandoc main.tex --to markdown --mathjax` to produce reviewer-ready
//!    Markdown quickly.
//! 4. Optionally, when `GROKRXIV_TEX_ENABLE_LATEXML=1`, run `latexml` →
//!    `latexmlpost` → `pandoc` to produce semantic XML + HTML5 + Markdown.
//!    Parse the XML into a `semantic_ast` JSON tree. If LaTeXML times out or
//!    errors, keep the pandoc Markdown.
//! 5. Parse the resulting Markdown into title / abstract / sections.
//!    Bibliography is harvested from `\bibitem` blocks in the original TeX
//!    plus any `.bib` files in the bundle.
//!
//! Env knobs:
//! - `GROKRXIV_PANDOC_BIN`             (default `pandoc`)
//! - `GROKRXIV_LATEXML_BIN`            (default `latexml`)
//! - `GROKRXIV_LATEXMLPOST_BIN`        (default `latexmlpost`)
//! - `GROKRXIV_LATEXML_TIMEOUT_SECS`   (default 20)
//! - `GROKRXIV_TEX_ENABLE_LATEXML=1`   opt into LaTeXML semantic AST
//! - `GROKRXIV_TEX_DISABLE_LATEXML=1`  force skip LaTeXML

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use flate2::read::GzDecoder;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use regex::Regex;
use serde_json::{json, Value};
use tar::Archive;
use tempfile::TempDir;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::types::{Citation, Section};

const ARXIV_SOURCE: &str = "https://arxiv.org/e-print/";

/// Result of pulling the source bundle and parsing it.
pub struct TexExtract {
    /// Title parsed from the rendered markdown / raw TeX.
    pub title: String,
    /// Abstract text from the rendered markdown / `\begin{abstract}` block.
    pub abstract_text: String,
    /// Body sections in document order.
    pub sections: Vec<Section>,
    /// Bibliography entries.
    pub bibliography: Vec<Citation>,
    /// LaTeXML-derived semantic AST (JSON). `None` if LaTeXML was unavailable.
    pub semantic_ast: Option<Value>,
}

/// Build the canonical source URL for an arXiv id.
pub fn source_url(arxiv_id: &str) -> String {
    format!("{ARXIV_SOURCE}{arxiv_id}")
}

/// Parse an arXiv source bundle into a [`TexExtract`] via a Pandoc + LaTeXML
/// subprocess pipeline.
///
/// Falls back to a plain `pandoc <main>.tex` invocation when LaTeXML is
/// disabled, missing, or times out. The bundle bytes can be tar.gz, plain
/// tar, gzipped single `.tex`, or raw UTF-8 `.tex`.
pub async fn parse_bundle(bytes: &Bytes) -> Result<TexExtract> {
    let files = unpack(bytes)?;
    if files.is_empty() {
        return Err(anyhow!("source bundle contained no .tex files"));
    }

    let tmp = TempDir::new().context("create temp dir for tex bundle")?;
    let layout = write_bundle(&tmp, &files)?;

    let main_path = layout.tex_dir.join(&layout.main_name);
    let raw_main = std::fs::read_to_string(&main_path)
        .with_context(|| format!("read main tex {main_path:?}"))?;

    let (markdown, semantic_ast) = run_conversion(&tmp, &layout).await;

    let title = extract_title(&markdown, &raw_main);
    let abstract_text = extract_abstract(&markdown, &raw_main);
    let sections = extract_md_sections(&markdown);
    let bibliography = collect_bibliography(&raw_main, &files);

    Ok(TexExtract {
        title,
        abstract_text,
        sections,
        bibliography,
        semantic_ast,
    })
}

// ---------------------------------------------------------------------------
// Bundle unpacking
// ---------------------------------------------------------------------------

/// Decode the raw bytes returned by arXiv's `e-print` endpoint into a
/// `{relpath → contents}` map of `.tex` and `.bib` files.
fn unpack(bytes: &Bytes) -> Result<HashMap<String, String>> {
    if let Ok(map) = try_targz(bytes) {
        if !map.is_empty() {
            return Ok(map);
        }
    }
    if let Ok(map) = try_tar(bytes) {
        if !map.is_empty() {
            return Ok(map);
        }
    }
    if let Ok(text) = try_gz_single(bytes) {
        let mut m = HashMap::new();
        m.insert("main.tex".to_string(), text);
        return Ok(m);
    }
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
    extract_text_files(&mut archive)
}

fn try_tar(bytes: &Bytes) -> Result<HashMap<String, String>> {
    let mut archive = Archive::new(bytes.as_ref());
    extract_text_files(&mut archive)
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

fn extract_text_files<R: Read>(archive: &mut Archive<R>) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        let path = entry.path().context("entry path")?.to_path_buf();
        let rel = path.to_string_lossy().to_string();
        let lower = rel.to_lowercase();
        if !(lower.ends_with(".tex") || lower.ends_with(".bib")) {
            continue;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_ok() {
            out.insert(rel, text);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Disk layout
// ---------------------------------------------------------------------------

struct BundleLayout {
    tex_dir: PathBuf,
    main_name: String,
}

fn write_bundle(tmp: &TempDir, files: &HashMap<String, String>) -> Result<BundleLayout> {
    let tex_dir = tmp.path().join("src");
    std::fs::create_dir_all(&tex_dir).context("create tex_dir")?;
    for (rel, contents) in files {
        let target = tex_dir.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&target, contents)
            .with_context(|| format!("write bundle file {target:?}"))?;
    }
    let main_name = pick_main(files);
    Ok(BundleLayout { tex_dir, main_name })
}

/// Prefer a file with BOTH `\documentclass` and `\begin{document}`; then
/// anything with `\documentclass`; then `main.tex`/`paper.tex`/`ms.tex`;
/// then the largest `.tex`.
fn pick_main(files: &HashMap<String, String>) -> String {
    for (name, contents) in files {
        if !name.to_lowercase().ends_with(".tex") {
            continue;
        }
        if contents.contains("\\documentclass") && contents.contains("\\begin{document}") {
            return name.clone();
        }
    }
    for (name, contents) in files {
        if !name.to_lowercase().ends_with(".tex") {
            continue;
        }
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
        .filter(|(k, _)| k.to_lowercase().ends_with(".tex"))
        .max_by_key(|(_, v)| v.len())
        .map(|(k, _)| k.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Subprocess pipeline
// ---------------------------------------------------------------------------

fn pandoc_bin() -> String {
    std::env::var("GROKRXIV_PANDOC_BIN").unwrap_or_else(|_| "pandoc".to_string())
}

fn latexml_bin() -> String {
    std::env::var("GROKRXIV_LATEXML_BIN").unwrap_or_else(|_| "latexml".to_string())
}

fn latexmlpost_bin() -> String {
    std::env::var("GROKRXIV_LATEXMLPOST_BIN").unwrap_or_else(|_| "latexmlpost".to_string())
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn latexml_enabled() -> bool {
    env_truthy("GROKRXIV_TEX_ENABLE_LATEXML") && !env_truthy("GROKRXIV_TEX_DISABLE_LATEXML")
}

fn subprocess_timeout() -> Duration {
    let secs = std::env::var("GROKRXIV_LATEXML_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(20);
    Duration::from_secs(secs)
}

async fn run_conversion(tmp: &TempDir, layout: &BundleLayout) -> (String, Option<Value>) {
    let timeout = subprocess_timeout();
    let xml_path = tmp.path().join("paper.xml");
    let html_path = tmp.path().join("paper.html");
    let media_path = tmp.path().join("media");

    let pandoc_markdown = match run_pandoc_tex(&layout.tex_dir, &layout.main_name, timeout).await {
        Ok(md) => md,
        Err(e) => {
            warn!(error = %e, "pandoc TeX conversion failed; returning empty markdown unless LaTeXML succeeds");
            String::new()
        }
    };

    if latexml_enabled() {
        match run_latexml_pipeline(
            &layout.tex_dir,
            &layout.main_name,
            &xml_path,
            &html_path,
            &media_path,
            timeout,
        )
        .await
        {
            Ok(markdown) => {
                let ast = parse_latexml_xml(&xml_path)
                    .map_err(|e| {
                        warn!(error = %e, "latexml xml→ast parse failed; semantic_ast unset");
                        e
                    })
                    .ok();
                return (markdown, ast);
            }
            Err(e) => {
                warn!(error = %e, "latexml pipeline failed; keeping pandoc markdown");
            }
        }
    }

    (pandoc_markdown, None)
}

async fn run_latexml_pipeline(
    tex_dir: &Path,
    main_name: &str,
    xml_path: &Path,
    html_path: &Path,
    media_path: &Path,
    timeout: Duration,
) -> Result<String> {
    debug!(?tex_dir, main_name, "latexml: stage 1");
    run_cmd(
        Command::new(latexml_bin())
            .current_dir(tex_dir)
            .arg("--quiet")
            .arg(format!("--destination={}", xml_path.display()))
            .arg(main_name),
        timeout,
        "latexml",
    )
    .await?;

    debug!(?xml_path, ?html_path, "latexml: stage 2 (latexmlpost)");
    run_cmd(
        Command::new(latexmlpost_bin())
            .arg("--quiet")
            .arg("--format=html5")
            .arg(format!("--destination={}", html_path.display()))
            .arg(xml_path),
        timeout,
        "latexmlpost",
    )
    .await?;

    debug!(?html_path, "latexml: stage 3 (pandoc html → markdown)");
    let out = run_cmd_capture(
        Command::new(pandoc_bin())
            .arg(html_path)
            .arg("--from=html")
            .arg("--to=markdown")
            .arg("--mathjax")
            .arg("--shift-heading-level-by=1")
            .arg(format!("--extract-media={}", media_path.display())),
        timeout,
        "pandoc(html→md)",
    )
    .await?;

    Ok(out)
}

async fn run_pandoc_tex(tex_dir: &Path, main_name: &str, timeout: Duration) -> Result<String> {
    let out = run_cmd_capture(
        Command::new(pandoc_bin())
            .current_dir(tex_dir)
            .arg(main_name)
            .arg("--from=latex")
            .arg("--to=markdown")
            .arg("--mathjax")
            .arg("--shift-heading-level-by=1"),
        timeout,
        "pandoc(tex→md)",
    )
    .await?;
    Ok(out)
}

async fn run_cmd(cmd: &mut Command, timeout: Duration, label: &str) -> Result<()> {
    cmd.kill_on_drop(true);
    let fut = cmd.output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| anyhow!("{label} timed out after {:?}", timeout))?
        .with_context(|| format!("spawn {label}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{label} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

async fn run_cmd_capture(cmd: &mut Command, timeout: Duration, label: &str) -> Result<String> {
    cmd.kill_on_drop(true);
    let fut = cmd.output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| anyhow!("{label} timed out after {:?}", timeout))?
        .with_context(|| format!("spawn {label}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{label} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout).context("non-utf8 pandoc output")?)
}

// ---------------------------------------------------------------------------
// LaTeXML XML → semantic AST JSON
// ---------------------------------------------------------------------------

/// Walk the LaTeXML XML output and build a JSON tree. Each node carries its
/// tag name, attributes, and either child nodes or a text payload.
fn parse_latexml_xml(path: &Path) -> Result<Value> {
    let xml =
        std::fs::read_to_string(path).with_context(|| format!("read latexml xml {path:?}"))?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut stack: Vec<(String, Vec<(String, String)>, Vec<Value>)> = Vec::new();
    stack.push(("__root__".to_string(), Vec::new(), Vec::new()));

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attrs = collect_attrs(&e);
                stack.push((tag, attrs, Vec::new()));
            }
            Ok(Event::End(_)) => {
                if stack.len() <= 1 {
                    break;
                }
                let (tag, attrs, children) = stack.pop().unwrap();
                let node = build_node(&tag, &attrs, children);
                stack.last_mut().unwrap().2.push(node);
            }
            Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attrs = collect_attrs(&e);
                let node = build_node(&tag, &attrs, Vec::new());
                stack.last_mut().unwrap().2.push(node);
            }
            Ok(Event::Text(t)) => {
                let text = t.unescape().unwrap_or_default().into_owned();
                if !text.trim().is_empty() {
                    stack.last_mut().unwrap().2.push(json!({"text": text}));
                }
            }
            Ok(Event::CData(t)) => {
                let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                if !text.trim().is_empty() {
                    stack.last_mut().unwrap().2.push(json!({"text": text}));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(anyhow!("xml parse error: {e}")),
        }
        buf.clear();
    }

    let (_, _, children) = stack.pop().unwrap();
    Ok(json!({"tag": "root", "children": children}))
}

fn collect_attrs(e: &quick_xml::events::BytesStart) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for attr in e.attributes().flatten() {
        let k = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let v = attr
            .unescape_value()
            .map(|c| c.into_owned())
            .unwrap_or_default();
        out.push((k, v));
    }
    out
}

fn build_node(tag: &str, attrs: &[(String, String)], children: Vec<Value>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("tag".to_string(), Value::String(tag.to_string()));
    if !attrs.is_empty() {
        let mut amap = serde_json::Map::new();
        for (k, v) in attrs {
            amap.insert(k.clone(), Value::String(v.clone()));
        }
        obj.insert("attrs".to_string(), Value::Object(amap));
    }
    if !children.is_empty() {
        obj.insert("children".to_string(), Value::Array(children));
    }
    Value::Object(obj)
}

// ---------------------------------------------------------------------------
// Title / abstract / section parsing
// ---------------------------------------------------------------------------

/// Title resolution: `\title{...}` in raw TeX → pandoc `% Title` line → empty.
fn extract_title(md: &str, raw_tex: &str) -> String {
    if let Some(t) = title_from_raw_tex(raw_tex) {
        return t;
    }
    for line in md.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("% ") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    String::new()
}

fn title_from_raw_tex(raw: &str) -> Option<String> {
    let re = Regex::new(r"(?s)\\title\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}").ok()?;
    let caps = re.captures(raw)?;
    let s = caps.get(1)?.as_str();
    let cleaned = sanitize_inline(s);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Abstract resolution: raw TeX `\begin{abstract}` → markdown `## Abstract`
/// section → first paragraph after `# Title`.
fn extract_abstract(md: &str, raw_tex: &str) -> String {
    if let Some(s) = abstract_from_raw_tex(raw_tex) {
        return s;
    }
    if let Some(s) = abstract_from_md_heading(md) {
        return s;
    }
    abstract_from_first_paragraph(md)
}

fn abstract_from_raw_tex(raw: &str) -> Option<String> {
    let re = Regex::new(r"(?s)\\begin\{abstract\}(.*?)\\end\{abstract\}").ok()?;
    let caps = re.captures(raw)?;
    let body = caps.get(1)?.as_str();
    let cleaned = sanitize_inline(body);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn abstract_from_md_heading(md: &str) -> Option<String> {
    let mut in_abstract = false;
    let mut buf = String::new();
    for line in md.lines() {
        let t = line.trim_start();
        let is_heading = t.starts_with("# ") || t.starts_with("## ") || t.starts_with("### ");
        if is_heading {
            if in_abstract {
                break;
            }
            let lower = t.to_lowercase();
            if lower.starts_with("## abstract") || lower.starts_with("# abstract") {
                in_abstract = true;
                continue;
            }
        }
        if in_abstract {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    let s = buf.trim();
    if s.is_empty() {
        None
    } else {
        Some(strip_md_inline(s))
    }
}

fn abstract_from_first_paragraph(md: &str) -> String {
    let mut past_title = false;
    let mut buf = String::new();
    for line in md.lines() {
        let t = line.trim();
        if !past_title {
            if t.starts_with("# ") {
                past_title = true;
            }
            continue;
        }
        if t.starts_with("## ") {
            break;
        }
        if t.is_empty() {
            if !buf.is_empty() {
                break;
            }
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    strip_md_inline(buf.trim())
}

/// Split markdown by `## Heading` lines into sections. We pass
/// `--shift-heading-level-by=1` to pandoc so `\section` always becomes `##`.
fn extract_md_sections(md: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if let Some((h, body)) = current.take() {
                sections.push(Section {
                    heading: h,
                    body_markdown: body.trim().to_string(),
                });
            }
            let mut h = heading.trim().trim_end_matches('#').trim().to_string();
            h = strip_md_inline(&h);
            if h.eq_ignore_ascii_case("abstract")
                || h.eq_ignore_ascii_case("references")
                || h.eq_ignore_ascii_case("bibliography")
            {
                current = None;
                continue;
            }
            current = Some((h, String::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((h, body)) = current {
        sections.push(Section {
            heading: h,
            body_markdown: body.trim().to_string(),
        });
    }
    sections
}

fn strip_md_inline(s: &str) -> String {
    let anchor = Regex::new(r"\s*\{#[^}]*\}").unwrap();
    let cleaned = anchor.replace_all(s, "").to_string();
    let multispace = Regex::new(r"\s+").unwrap();
    multispace.replace_all(cleaned.trim(), " ").to_string()
}

// ---------------------------------------------------------------------------
// Bibliography
// ---------------------------------------------------------------------------

fn collect_bibliography(raw_main: &str, files: &HashMap<String, String>) -> Vec<Citation> {
    let mut out = parse_bibitems(raw_main);
    for (name, contents) in files {
        if !name.to_lowercase().ends_with(".bib") {
            continue;
        }
        out.extend(parse_bibfile(contents));
    }
    out
}

fn parse_bibitems(src: &str) -> Vec<Citation> {
    let re = Regex::new(r"\\bibitem(?:\[[^\]]*\])?\s*\{([^}]+)\}").unwrap();
    let mut entries: Vec<(usize, usize, String)> = re
        .captures_iter(src)
        .map(|c| {
            let key = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let header_start = c.get(0).unwrap().start();
            let content_start = c.get(0).unwrap().end();
            (header_start, content_start, key)
        })
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let bib_end = src.find("\\end{thebibliography}").unwrap_or(src.len());
    entries.sort_by_key(|t| t.0);

    let mut out = Vec::new();
    for (i, (_, content_start, key)) in entries.iter().enumerate() {
        let content_end = entries.get(i + 1).map(|t| t.0).unwrap_or(bib_end);
        if content_end <= *content_start {
            continue;
        }
        let raw = src[*content_start..content_end].trim();
        if raw.is_empty() {
            continue;
        }
        let raw_clean = sanitize_inline(raw);
        let (doi, arxiv_id) = sniff_identifiers(&raw_clean);
        out.push(Citation {
            raw: format!("{key}: {raw_clean}"),
            doi,
            arxiv_id,
            title: Some(key.clone()),
        });
    }
    out
}

fn parse_bibfile(src: &str) -> Vec<Citation> {
    let re = Regex::new(r"(?s)@\w+\s*\{\s*([^,\s]+)\s*,(.*?)\n\s*\}\s*\n").unwrap();
    let mut out = Vec::new();
    for c in re.captures_iter(src) {
        let key = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let body = c.get(2).map(|m| m.as_str()).unwrap_or("");
        let raw_clean = sanitize_inline(body);
        let (doi, arxiv_id) = sniff_identifiers(&raw_clean);
        let title = extract_bib_field(body, "title");
        out.push(Citation {
            raw: if raw_clean.is_empty() {
                key.clone()
            } else {
                format!("{key}: {raw_clean}")
            },
            doi,
            arxiv_id,
            title: title.or(Some(key)),
        });
    }
    out
}

fn extract_bib_field(body: &str, field: &str) -> Option<String> {
    let pattern = format!(r#"(?i){field}\s*=\s*[{{"]([^}}"]+)[}}"]"#);
    let re = Regex::new(&pattern).ok()?;
    re.captures(body)
        .and_then(|c| c.get(1).map(|m| sanitize_inline(m.as_str())))
}

fn sniff_identifiers(text: &str) -> (Option<String>, Option<String>) {
    let doi = Regex::new(r"\b10\.\d{4,9}/[-._;()/:A-Za-z0-9]+")
        .unwrap()
        .find(text)
        .map(|m| {
            m.as_str()
                .trim_end_matches(&[',', '.', ';'][..])
                .to_string()
        });
    let arxiv = Regex::new(r"\b(?:arXiv:)?(\d{4}\.\d{4,5})(?:v\d+)?")
        .unwrap()
        .captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    (doi, arxiv)
}

fn sanitize_inline(s: &str) -> String {
    let mut t = s.to_string();
    let wrap = Regex::new(
        r"\\(?:textbf|textit|emph|underline|texttt|textrm|mathrm|mathit|mathbf|mathsf|mathcal|operatorname)\s*\{([^{}]*)\}",
    )
    .unwrap();
    for _ in 0..3 {
        t = wrap.replace_all(&t, "$1").to_string();
    }
    // Strip explicit line-break commands: `\\`, `\\[0.5em]`, `\\*`. These show
    // up in `\title{...}` for two-line/decorated titles and would otherwise
    // surface literally to the web UI.
    let linebreaks = Regex::new(r"\\\\(?:\*?\s*\[[^\]]*\])?").unwrap();
    t = linebreaks.replace_all(&t, " ").to_string();
    // Strip bare formatting commands (no braces): font sizes, weight, family,
    // alignment markers. Word-boundary on the right so `\largesomething` isn't
    // mangled.
    let bare_commands = Regex::new(
        r"\\(?:large|Large|LARGE|normalsize|small|footnotesize|scriptsize|tiny|huge|Huge|bfseries|itshape|slshape|scshape|upshape|mdseries|rmfamily|sffamily|ttfamily|centering|raggedright|raggedleft|noindent|indent|newline|linebreak|hfill|vfill|par|smallskip|medskip|bigskip|allowbreak)\b",
    )
    .unwrap();
    t = bare_commands.replace_all(&t, " ").to_string();
    t = t.replace('\n', " ").replace('~', " ");
    let multispace = Regex::new(r"\s+").unwrap();
    multispace.replace_all(&t, " ").trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEX_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TexEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl TexEnvGuard {
        fn new() -> Self {
            const KEYS: &[&str] = &[
                "GROKRXIV_TEX_ENABLE_LATEXML",
                "GROKRXIV_TEX_DISABLE_LATEXML",
                "GROKRXIV_LATEXML_BIN",
                "GROKRXIV_LATEXMLPOST_BIN",
            ];
            let lock = TEX_ENV_LOCK.lock().unwrap();
            let saved = KEYS
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in KEYS {
                std::env::remove_var(key);
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for TexEnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn have_bin(name: &str) -> bool {
        let path = match std::env::var_os("PATH") {
            Some(p) => p,
            None => return false,
        };
        std::env::split_paths(&path).any(|d| d.join(name).is_file())
    }

    fn make_targz(files: &[(&str, &str)]) -> Bytes {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            for (name, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, contents.as_bytes()).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz_buf = Vec::new();
        {
            let mut enc = GzEncoder::new(&mut gz_buf, Compression::default());
            enc.write_all(&tar_buf).unwrap();
            enc.finish().unwrap();
        }
        Bytes::from(gz_buf)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pandoc_smoke() {
        let _env = TexEnvGuard::new();
        if !have_bin(&pandoc_bin()) {
            eprintln!("skipping: pandoc not installed");
            return;
        }
        std::env::set_var("GROKRXIV_TEX_DISABLE_LATEXML", "1");
        let tex = r#"\documentclass{article}
\begin{document}
\section{Intro}
Some text.
\end{document}
"#;
        let bundle = make_targz(&[("main.tex", tex)]);
        let extract = parse_bundle(&bundle).await.expect("parse_bundle");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
        assert!(
            extract
                .sections
                .iter()
                .any(|s| s.heading.eq_ignore_ascii_case("Intro")),
            "expected Intro section, got: {:?}",
            extract
                .sections
                .iter()
                .map(|s| &s.heading)
                .collect::<Vec<_>>()
        );
        let body_joined: String = extract
            .sections
            .iter()
            .map(|s| s.body_markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body_joined.contains("Some text"),
            "expected body to contain 'Some text', body was: {body_joined}"
        );
        assert!(
            extract.semantic_ast.is_none(),
            "semantic_ast should be None on pandoc-only path"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn latexml_preserves_theorem() {
        let _env = TexEnvGuard::new();
        if !have_bin(&pandoc_bin()) || !have_bin(&latexml_bin()) {
            eprintln!("skipping: latexml not installed");
            return;
        }
        std::env::set_var("GROKRXIV_TEX_ENABLE_LATEXML", "1");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
        let tex = r#"\documentclass{article}
\usepackage{amsthm}
\newtheorem{theorem}{Theorem}
\begin{document}
\section{Intro}
\begin{theorem}
Let $n \ge 1$.
\end{theorem}
\end{document}
"#;
        let bundle = make_targz(&[("main.tex", tex)]);
        let extract = parse_bundle(&bundle).await.expect("parse_bundle");
        std::env::remove_var("GROKRXIV_TEX_ENABLE_LATEXML");
        let body_joined: String = extract
            .sections
            .iter()
            .map(|s| s.body_markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let theorem_in_md = body_joined.contains("Let")
            && (body_joined.contains("n ≥ 1")
                || body_joined.contains("n \\ge 1")
                || body_joined.contains("\\geq"));
        let theorem_in_ast = extract
            .semantic_ast
            .as_ref()
            .map(|v| {
                let s = v.to_string();
                s.contains("theorem") || s.contains("Theorem")
            })
            .unwrap_or(false);
        assert!(
            theorem_in_md || theorem_in_ast,
            "expected theorem to be detectable in markdown or semantic_ast"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pandoc_fallback_when_latexml_disabled() {
        let _env = TexEnvGuard::new();
        if !have_bin(&pandoc_bin()) {
            eprintln!("skipping: pandoc not installed");
            return;
        }
        std::env::set_var("GROKRXIV_TEX_DISABLE_LATEXML", "1");
        // Point latexml at a path that doesn't exist — if the pipeline ever
        // tries to spawn it, the call would surface as a (caught) error, but
        // semantic_ast would remain None and the pandoc fallback would still
        // produce the markdown. The real assertion is that semantic_ast is
        // None, indicating latexml never produced an XML AST.
        std::env::set_var(
            "GROKRXIV_LATEXML_BIN",
            "/nonexistent/latexml-should-not-be-spawned",
        );
        std::env::set_var(
            "GROKRXIV_LATEXMLPOST_BIN",
            "/nonexistent/latexmlpost-should-not-be-spawned",
        );
        let tex = r#"\documentclass{article}
\begin{document}
\section{Hello}
World.
\end{document}
"#;
        let bundle = make_targz(&[("main.tex", tex)]);
        let extract = parse_bundle(&bundle).await.expect("parse_bundle");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
        std::env::remove_var("GROKRXIV_LATEXML_BIN");
        std::env::remove_var("GROKRXIV_LATEXMLPOST_BIN");
        assert!(extract.semantic_ast.is_none(), "semantic_ast must be None");
        assert!(
            extract
                .sections
                .iter()
                .any(|s| s.heading.eq_ignore_ascii_case("Hello")),
            "expected Hello section, got: {:?}",
            extract
                .sections
                .iter()
                .map(|s| &s.heading)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bibliography_parses_from_bibitem() {
        let _env = TexEnvGuard::new();
        if !have_bin(&pandoc_bin()) {
            eprintln!("skipping: pandoc not installed");
            return;
        }
        std::env::set_var("GROKRXIV_TEX_DISABLE_LATEXML", "1");
        let tex = r#"\documentclass{article}
\begin{document}
\section{Intro}
See~\cite{foo}.
\begin{thebibliography}{99}
\bibitem{foo}A. Foo, A useful paper, 2020.
\end{thebibliography}
\end{document}
"#;
        let bundle = make_targz(&[("main.tex", tex)]);
        let extract = parse_bundle(&bundle).await.expect("parse_bundle");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
        assert!(
            !extract.bibliography.is_empty(),
            "bibliography should not be empty"
        );
        assert_eq!(extract.bibliography[0].title.as_deref(), Some("foo"));
        assert!(
            extract.bibliography[0].raw.starts_with("foo:")
                || extract.bibliography[0].raw.contains("A. Foo"),
            "bibliography raw should reference 'foo'/'A. Foo': {}",
            extract.bibliography[0].raw
        );
    }

    #[test]
    fn latexml_is_opt_in_by_default() {
        let _env = TexEnvGuard::new();
        std::env::remove_var("GROKRXIV_TEX_ENABLE_LATEXML");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
        assert!(
            !latexml_enabled(),
            "LaTeXML must not run by default on the operator CLI path"
        );

        std::env::set_var("GROKRXIV_TEX_ENABLE_LATEXML", "1");
        assert!(latexml_enabled(), "explicit opt-in should enable LaTeXML");

        std::env::set_var("GROKRXIV_TEX_DISABLE_LATEXML", "1");
        assert!(
            !latexml_enabled(),
            "explicit disable should override the opt-in"
        );

        std::env::remove_var("GROKRXIV_TEX_ENABLE_LATEXML");
        std::env::remove_var("GROKRXIV_TEX_DISABLE_LATEXML");
    }

    #[test]
    fn picks_main_by_documentclass() {
        let mut files = HashMap::new();
        files.insert("appendix.tex".to_string(), "no class here".to_string());
        files.insert(
            "paper.tex".to_string(),
            "\\documentclass{article}\n\\begin{document}\nx\n\\end{document}".to_string(),
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
    fn sanitize_inline_strips_latex_decorations_from_titles() {
        // Real-world title from arXiv:2605.12239 — the LaTeX source uses \\,
        // \large, and \\[0.5em] to render a two-line decorated title.
        let raw = "Harness Engineering as Categorical Architecture\\\\ \\large Structural Guarantees Are Harness-Level Properties\\\\[0.5em] \\normalsize Preprint -- Feedback Welcome";
        let cleaned = sanitize_inline(raw);
        assert_eq!(
            cleaned,
            "Harness Engineering as Categorical Architecture Structural Guarantees Are Harness-Level Properties Preprint -- Feedback Welcome"
        );
    }

    #[test]
    fn sanitize_inline_strips_wrapper_commands() {
        assert_eq!(sanitize_inline("\\textbf{Hello} World"), "Hello World");
        assert_eq!(
            sanitize_inline("\\emph{italic} text \\mathrm{x}"),
            "italic text x"
        );
    }

    /// Regression: a paper with N H2 headings must yield N section entries.
    /// We've seen Pandoc-rendered bodies where 9 H2 headings collapsed to 2
    /// section entries because the splitter swallowed lines or stopped at
    /// the first heading match. Use a synthetic body that mirrors what
    /// Pandoc emits (heading lines `## Heading`, body paragraphs, blanks).
    #[test]
    fn extract_md_sections_one_per_h2_heading() {
        let md = "## Introduction\nIntro body.\n\n\
                  ## Generalized Fourier Transform on Riemannian Manifold\nbody2.\n\n\
                  ## Spectral Degeneracy and Operator Freedom\nbody3.\n\n\
                  ## Geometric Operators and Symmetry-Adapted Bases\nbody4.\n\n\
                  ## Gauge and Coordinate Freedom in GFT\nbody5.\n\n\
                  ## GFT Classifications\nbody6.\n\n\
                  ## Subtleties and Examples\nbody7.\n\n\
                  ## Discussions and Summary\nbody8.\n\n\
                  ## Acknowledgement\nbody9.\n";
        let sections = super::extract_md_sections(md);
        let h2_count = md.lines().filter(|l| l.starts_with("## ")).count();
        assert_eq!(h2_count, 9, "fixture should have 9 H2 lines");
        assert_eq!(
            sections.len(),
            h2_count,
            "expected {h2_count} sections (one per H2), got {}: {:?}",
            sections.len(),
            sections
                .iter()
                .map(|s| s.heading.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(sections[0].heading, "Introduction");
        assert_eq!(sections[8].heading, "Acknowledgement");
        assert!(sections[0].body_markdown.contains("Intro body"));
    }

    /// Regression: filtered headings (abstract, references, bibliography) are
    /// excluded but they must not eat the *next* heading's content.
    #[test]
    fn extract_md_sections_skips_filtered_without_swallowing_next() {
        let md = "## Abstract\nabstract body.\n\n\
                  ## Introduction\nintro body.\n\n\
                  ## References\nreferences body.\n\n\
                  ## Conclusion\nconclusion body.\n";
        let sections = super::extract_md_sections(md);
        let headings: Vec<&str> = sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, vec!["Introduction", "Conclusion"]);
        assert!(sections[0].body_markdown.contains("intro body"));
        assert!(sections[1].body_markdown.contains("conclusion body"));
    }

    #[test]
    fn bibfile_parses_basic_entry() {
        let bib = "@article{key1,\n  title = {A Paper},\n  author = {J. Doe},\n  doi = {10.1000/xyz},\n}\n";
        let cites = parse_bibfile(bib);
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0].title.as_deref(), Some("A Paper"));
        assert_eq!(cites[0].doi.as_deref(), Some("10.1000/xyz"));
    }
}
