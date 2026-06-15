//! Tools specific to the `TheoremGraphExtractorAgent` (Stage 6).
//!
//! Three tools live here:
//!
//! - `list_sections()` — enumerate the paper's section structure (from the
//!   semantic AST when available, otherwise by scanning `body.md` for ATX
//!   headings).
//! - `read_section(id)` — return the body slice of a single section, plus a
//!   best-effort scan for theorem-like blocks inside it.
//! - `resolve_label(label)` — turn a `\ref{}` target into the actual theorem /
//!   lemma / equation / section it points to, with its source location.
//!
//! The implementations are deliberately deterministic and small. The LLM owns
//! the cross-reference reasoning; these tools just surface ground truth.
//!
//! ## Markdown theorem-block detection
//!
//! When `semantic_ast` is absent (VLM / PDF ingest path), we fall back to
//! regex-based detection over `body.md`. The patterns we recognise:
//!
//! 1. **LaTeX-flavoured markdown**:
//!    `\begin{theorem}[Optional title]\label{thm:foo} BODY \end{theorem}`
//!    (also `lemma`, `proposition`, `corollary`, `definition`, `proof`,
//!     `remark`, `construction`, `example`).
//! 2. **Bold-prefix markdown**:
//!    `**Theorem 2.1.** STATEMENT` or `**Theorem 2.1 (Title).** STATEMENT`
//!    (case-insensitive on the type word; one line of statement preview).
//! 3. **Heading-prefix markdown**:
//!    `### Theorem 2.1` followed by free-form body until the next heading.
//!
//! These are the THREE flavours covered. Anything else falls through.

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::extraction::{Tool, ToolCtx};

// ---------------------------------------------------------------------------
// list_sections
// ---------------------------------------------------------------------------

/// Implements `list_sections`.
pub struct ListSectionsTool;

static LIST_SECTIONS_SCHEMA: OnceLock<Value> = OnceLock::new();

fn list_sections_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

#[async_trait]
impl Tool for ListSectionsTool {
    fn name(&self) -> &'static str {
        "list_sections"
    }
    fn description(&self) -> &'static str {
        "Enumerate the paper's section structure. Returns [{id, heading, level, char_start, char_end}] in document order."
    }
    fn schema(&self) -> &Value {
        LIST_SECTIONS_SCHEMA.get_or_init(list_sections_schema)
    }
    async fn call(&self, _args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let sections = list_sections(ctx)?;
        Ok(json!({ "sections": sections }))
    }
}

/// One section descriptor surfaced by `list_sections`.
#[derive(Debug, Clone)]
pub struct SectionEntry {
    /// Stable identifier for the section (e.g. `sec-1`, `sec-2-1`).
    pub id: String,
    /// Heading text (without leading `#` hashes).
    pub heading: String,
    /// Heading depth (1 = top-level `#`, 2 = `##`, ...).
    pub level: u32,
    /// Byte offset into `body.md` where this section starts (inclusive).
    pub char_start: usize,
    /// Byte offset into `body.md` where this section ends (exclusive).
    pub char_end: usize,
}

impl SectionEntry {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "heading": self.heading,
            "level": self.level,
            "char_start": self.char_start,
            "char_end": self.char_end,
        })
    }
}

/// Build the section list from either the semantic AST (preferred) or the
/// markdown body. Pure function — testable without a real `ToolCtx`.
pub fn list_sections(ctx: &ToolCtx<'_>) -> anyhow::Result<Vec<Value>> {
    let entries = if let Some(ast) = ctx.semantic_ast {
        sections_from_ast(ast)
    } else {
        sections_from_markdown(&read_body_md(ctx)?)
    };
    Ok(entries.iter().map(SectionEntry::to_json).collect())
}

fn read_body_md(ctx: &ToolCtx<'_>) -> anyhow::Result<String> {
    let p = ctx.workdir.join("body.md");
    if !p.exists() {
        // Nothing to scan — return empty body. The agent will get an empty
        // section list, which is honest.
        return Ok(String::new());
    }
    let bytes = std::fs::read(&p)
        .map_err(|e| anyhow::anyhow!("list_sections: could not read body.md: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Walk a semantic AST and pull out section-like nodes. Supports either a
/// `{kind: "section", ...}` shape (LaTeXML-flavoured) or any object with a
/// `tag: "section"` field. Children are recursed.
pub fn sections_from_ast(ast: &Value) -> Vec<SectionEntry> {
    let mut out: Vec<SectionEntry> = Vec::new();
    let mut counter: Vec<u32> = vec![0]; // path counter at each level
    walk_ast(ast, 1, &mut counter, &mut out);
    out
}

fn walk_ast(node: &Value, level: u32, counter: &mut Vec<u32>, out: &mut Vec<SectionEntry>) {
    match node {
        Value::Object(map) => {
            let kind = map
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| map.get("tag").and_then(Value::as_str))
                .unwrap_or("");
            let is_section = matches!(
                kind,
                "section" | "subsection" | "subsubsection" | "paragraph"
            );
            if is_section {
                while counter.len() <= level as usize {
                    counter.push(0);
                }
                counter[level as usize - 1] += 1;
                // Reset any deeper counters when we enter a new sibling.
                for c in counter.iter_mut().skip(level as usize) {
                    *c = 0;
                }
                let id = map
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| map.get("xml:id").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| {
                        let path: Vec<String> = counter[..level as usize]
                            .iter()
                            .filter(|n| **n > 0)
                            .map(u32::to_string)
                            .collect();
                        format!("sec-{}", path.join("-"))
                    });
                let heading = map
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| map.get("heading").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let char_start = map
                    .get("char_start")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
                    .unwrap_or(0);
                let char_end = map
                    .get("char_end")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
                    .unwrap_or(0);
                let resolved_level = match kind {
                    "section" => 1,
                    "subsection" => 2,
                    "subsubsection" => 3,
                    "paragraph" => 4,
                    _ => level,
                };
                out.push(SectionEntry {
                    id,
                    heading,
                    level: resolved_level,
                    char_start,
                    char_end,
                });
                if let Some(children) = map.get("children") {
                    walk_ast(children, resolved_level + 1, counter, out);
                }
                return;
            }
            // Not a section node — recurse into children / arbitrary fields.
            if let Some(children) = map.get("children") {
                walk_ast(children, level, counter, out);
            } else {
                for v in map.values() {
                    walk_ast(v, level, counter, out);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_ast(v, level, counter, out);
            }
        }
        _ => {}
    }
}

/// Fallback: scan markdown for ATX headings (`# ...` through `###### ...`) and
/// emit a section per heading. Section ids encode the heading hierarchy as
/// `sec-1`, `sec-1-1`, `sec-1-2-3`, etc.
pub fn sections_from_markdown(body: &str) -> Vec<SectionEntry> {
    let mut out: Vec<SectionEntry> = Vec::new();
    let mut counters: [u32; 6] = [0; 6];
    let mut pending: Option<(usize, u32, String, [u32; 6])> = None; // (start_byte, level, heading, snapshot)

    let mut byte_cursor: usize = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let leading_hashes = trimmed.chars().take_while(|&c| c == '#').count();
        let after = if leading_hashes > 0 {
            &trimmed[leading_hashes..]
        } else {
            ""
        };
        let is_heading = (1..=6).contains(&leading_hashes) && after.starts_with(' ');
        if is_heading {
            let level = leading_hashes as u32;
            let heading = after.trim().trim_end_matches('\n').to_string();
            // Bump the counter at this level + reset deeper ones.
            counters[level as usize - 1] += 1;
            for c in &mut counters[level as usize..] {
                *c = 0;
            }
            // Close out the previous heading.
            if let Some((prev_start, prev_level, prev_heading, prev_counters)) = pending.take() {
                out.push(SectionEntry {
                    id: id_from_counters(&prev_counters, prev_level),
                    heading: prev_heading,
                    level: prev_level,
                    char_start: prev_start,
                    char_end: byte_cursor,
                });
            }
            pending = Some((byte_cursor, level, heading, counters));
        }
        byte_cursor += line.len();
    }
    if let Some((start, level, heading, counters)) = pending {
        out.push(SectionEntry {
            id: id_from_counters(&counters, level),
            heading,
            level,
            char_start: start,
            char_end: byte_cursor,
        });
    }
    out
}

fn id_from_counters(counters: &[u32; 6], level: u32) -> String {
    let path: Vec<String> = counters[..level as usize]
        .iter()
        .filter(|n| **n > 0)
        .map(u32::to_string)
        .collect();
    if path.is_empty() {
        "sec-1".to_string()
    } else {
        format!("sec-{}", path.join("-"))
    }
}

// ---------------------------------------------------------------------------
// read_section
// ---------------------------------------------------------------------------

/// Implements `read_section`.
pub struct ReadSectionTool;

static READ_SECTION_SCHEMA: OnceLock<Value> = OnceLock::new();

fn read_section_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Section id as returned by list_sections (e.g., `sec-1`, `sec-2-1`)."
            }
        }
    })
}

#[async_trait]
impl Tool for ReadSectionTool {
    fn name(&self) -> &'static str {
        "read_section"
    }
    fn description(&self) -> &'static str {
        "Return the body slice + theorem-like blocks inside a specific section. Pair with list_sections to navigate the paper."
    }
    fn schema(&self) -> &Value {
        READ_SECTION_SCHEMA.get_or_init(read_section_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("read_section requires `id`"))?;
        read_section(id, ctx)
    }
}

/// Read a single section by id. Pure function — testable.
pub fn read_section(id: &str, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
    let body = read_body_md(ctx)?;
    let sections: Vec<SectionEntry> = if let Some(ast) = ctx.semantic_ast {
        sections_from_ast(ast)
    } else {
        sections_from_markdown(&body)
    };
    let section = sections
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow::anyhow!("read_section: unknown section id `{id}`"))?;

    let slice = if !body.is_empty() && section.char_end > section.char_start {
        let end = section.char_end.min(body.len());
        let start = section.char_start.min(end);
        body[start..end].to_string()
    } else {
        String::new()
    };
    let theorems = scan_theorem_blocks(&slice);
    Ok(json!({
        "heading": section.heading,
        "level": section.level,
        "char_start": section.char_start,
        "char_end": section.char_end,
        "body_markdown": slice,
        "theorems": theorems,
    }))
}

/// One detected theorem-like block.
#[derive(Debug, Clone)]
pub struct TheoremBlock {
    /// `\label{...}` value if any, else `None`.
    pub label: Option<String>,
    /// Kind word (theorem / lemma / proposition / corollary / ...).
    pub kind: String,
    /// Full extracted statement text. Never ellipsized.
    pub statement: String,
    /// First ~200 chars of the statement.
    pub statement_preview: String,
    /// Raw TeX/LaTeX block when the source was an explicit environment.
    pub source_tex: Option<String>,
}

impl TheoremBlock {
    fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "type": self.kind,
            "statement": self.statement,
            "statement_preview": self.statement_preview,
            "source_tex": self.source_tex.as_deref(),
        })
    }
}

const PREVIEW_LEN: usize = 200;

static TEX_ENV_RES: OnceLock<Vec<(String, Regex)>> = OnceLock::new();
static MD_BOLD_RE: OnceLock<Regex> = OnceLock::new();
static MD_HEADING_RE: OnceLock<Regex> = OnceLock::new();
static MD_TITLE_RE: OnceLock<Regex> = OnceLock::new();

const TEX_ENV_KINDS: &[&str] = &[
    "theorem",
    "lemma",
    "proposition",
    "corollary",
    "definition",
    "proof",
    "remark",
    "construction",
    "example",
];

/// One regex per environment kind. The Rust `regex` crate doesn't support
/// backreferences, so we can't write a single regex with `\1` to match the
/// closing `\end{kind}` — instead we compile one regex per kind with the kind
/// name baked in.
fn tex_env_res() -> &'static Vec<(String, Regex)> {
    TEX_ENV_RES.get_or_init(|| {
        TEX_ENV_KINDS
            .iter()
            .map(|kind| {
                let pat = format!(
                    r"(?is)\\begin\{{{kind}\}}(?:\s*\[[^\]]*\])?\s*(?:\\label\{{(?P<label>[^}}]+)\}})?(?P<body>.*?)\\end\{{{kind}\}}",
                    kind = kind
                );
                (kind.to_string(), Regex::new(&pat).expect("valid regex"))
            })
            .collect()
    })
}

fn md_bold_re() -> &'static Regex {
    MD_BOLD_RE.get_or_init(|| {
        // **Theorem 2.1.** body  /  **Theorem 2.1 (Title).** body
        Regex::new(
            r"(?im)^\*\*(?P<kind>Theorem|Lemma|Proposition|Corollary|Definition|Proof|Remark|Example)\s*(?P<num>[0-9][0-9A-Za-z.\-]*)?(?:\s*\([^)]*\))?\.\*\*\s*(?P<body>.+)$",
        )
        .expect("valid regex")
    })
}

fn md_heading_re() -> &'static Regex {
    MD_HEADING_RE.get_or_init(|| {
        // ### Theorem 2.1  ...   (heading line; body follows on subsequent lines until next heading)
        Regex::new(
            r"(?im)^(?P<hashes>\#{1,6})\s+(?P<kind>Theorem|Lemma|Proposition|Corollary|Definition|Proof|Remark|Example)\b(?P<rest>.*)$",
        )
        .expect("valid regex")
    })
}

fn md_title_re() -> &'static Regex {
    MD_TITLE_RE.get_or_init(|| {
        // ### Spectral Decomposition Theorem / ##### Proof sketch.
        Regex::new(
            r"(?im)^(?P<hashes>\#{1,6})\s+(?P<title>.*\b(?:Theorem|Lemma|Proposition|Corollary|Definition|Proof)\b.*)$",
        )
        .expect("valid regex")
    })
}

/// Scan a markdown / latex-flavoured-markdown body for theorem-like blocks.
/// Supports the three patterns documented at the top of this file.
pub fn scan_theorem_blocks(body: &str) -> Vec<Value> {
    let mut out: Vec<TheoremBlock> = Vec::new();

    // 1) TeX-style environments — one regex per kind because regex backrefs
    //    aren't supported.
    for (kind_name, re) in tex_env_res() {
        for cap in re.captures_iter(body) {
            let label = cap.name("label").map(|m| m.as_str().to_string());
            let body_text = cap.name("body").map(|m| m.as_str()).unwrap_or("").trim();
            out.push(TheoremBlock {
                label,
                kind: kind_name.clone(),
                statement: body_text.to_string(),
                statement_preview: preview(body_text),
                source_tex: cap.get(0).map(|m| m.as_str().to_string()),
            });
        }
    }

    // 2) Bold-prefix.
    for cap in md_bold_re().captures_iter(body) {
        let kind = cap
            .name("kind")
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default();
        let stmt = cap.name("body").map(|m| m.as_str()).unwrap_or("").trim();
        out.push(TheoremBlock {
            label: None,
            kind,
            statement: stmt.to_string(),
            statement_preview: preview(stmt),
            source_tex: None,
        });
    }

    // 3) Heading-prefix. We grab the rest of the line plus, when present, the
    //    paragraph that follows the heading.
    for cap in md_heading_re().captures_iter(body) {
        let kind = cap
            .name("kind")
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default();
        // Find the body that follows the heading line.
        let full_match_end = cap.get(0).map(|m| m.end()).unwrap_or(0);
        let tail = &body[full_match_end..];
        let next_para = tail
            .lines()
            .skip_while(|l| l.trim().is_empty())
            .take_while(|l| !l.starts_with('#') && !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let statement = next_para.trim().to_string();
        out.push(TheoremBlock {
            label: None,
            kind,
            statement_preview: preview(&statement),
            statement,
            source_tex: None,
        });
    }

    // 4) Title-containing headings such as "Spectral Decomposition Theorem"
    // or "Proof sketch." Pandoc often emits those for theorem-like prose even
    // when the source did not use a theorem environment.
    for cap in md_title_re().captures_iter(body) {
        let title = cap.name("title").map(|m| m.as_str()).unwrap_or("").trim();
        if starts_with_kind_word(title) {
            continue;
        }
        let Some(kind) = kind_from_title(title) else {
            continue;
        };
        let full_match_end = cap.get(0).map(|m| m.end()).unwrap_or(0);
        let tail = &body[full_match_end..];
        let next_para = tail
            .lines()
            .skip_while(|l| l.trim().is_empty())
            .take_while(|l| !l.starts_with('#') && !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let statement = if next_para.trim().is_empty() {
            title
        } else {
            next_para.trim()
        };
        let statement = statement.to_string();
        out.push(TheoremBlock {
            label: None,
            kind,
            statement_preview: preview(&statement),
            statement,
            source_tex: None,
        });
    }

    out.iter().map(TheoremBlock::to_json).collect()
}

fn starts_with_kind_word(title: &str) -> bool {
    let lower = title.trim_start().to_lowercase();
    [
        "theorem",
        "lemma",
        "proposition",
        "corollary",
        "definition",
        "proof",
        "remark",
        "example",
    ]
    .iter()
    .any(|kind| lower.starts_with(kind))
}

fn kind_from_title(title: &str) -> Option<String> {
    let lower = title.to_lowercase();
    for kind in [
        "theorem",
        "lemma",
        "proposition",
        "corollary",
        "definition",
        "proof",
    ] {
        if lower.contains(kind) {
            return Some(kind.to_string());
        }
    }
    None
}

fn preview(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= PREVIEW_LEN {
        return trimmed.to_string();
    }
    let cutoff: String = trimmed.chars().take(PREVIEW_LEN).collect();
    format!("{cutoff}...")
}

// ---------------------------------------------------------------------------
// resolve_label
// ---------------------------------------------------------------------------

/// Implements `resolve_label`.
pub struct ResolveLabelTool;

static RESOLVE_LABEL_SCHEMA: OnceLock<Value> = OnceLock::new();

fn resolve_label_schema() -> Value {
    json!({
        "type": "object",
        "required": ["label"],
        "properties": {
            "label": {
                "type": "string",
                "description": "Target of a \\ref{}, e.g. `thm:foo`. Returns kind + id + location."
            }
        }
    })
}

#[async_trait]
impl Tool for ResolveLabelTool {
    fn name(&self) -> &'static str {
        "resolve_label"
    }
    fn description(&self) -> &'static str {
        "Resolve a \\ref{} target to the kind of block it labels (theorem/lemma/equation/section), its stable id, and its location."
    }
    fn schema(&self) -> &Value {
        RESOLVE_LABEL_SCHEMA.get_or_init(resolve_label_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let label = args
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("resolve_label requires `label`"))?;
        Ok(resolve_label(label, ctx)?)
    }
}

/// Resolve a label. AST-first; falls back to markdown scan.
pub fn resolve_label(label: &str, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
    if let Some(ast) = ctx.semantic_ast {
        if let Some(found) = resolve_label_in_ast(label, ast) {
            return Ok(found);
        }
    }
    let body = read_body_md(ctx)?;
    if !body.is_empty() {
        if let Some(found) = resolve_label_in_markdown(label, &body, ctx) {
            return Ok(found);
        }
    }
    Ok(json!({
        "kind": "unknown",
        "id": label,
        "location": "",
    }))
}

/// Search an AST for `{labels: ["thm:foo"]}` or `{label: "thm:foo"}` fields
/// attached to a node, and return the kind / id / xml:id / char_start.
fn resolve_label_in_ast(label: &str, ast: &Value) -> Option<Value> {
    let mut path: Vec<String> = Vec::new();
    let mut found: Option<Value> = None;
    search_label_in_ast(label, ast, &mut path, &mut found);
    found
}

fn search_label_in_ast(
    target: &str,
    node: &Value,
    path: &mut Vec<String>,
    found: &mut Option<Value>,
) {
    if found.is_some() {
        return;
    }
    match node {
        Value::Object(map) => {
            let has_label = map
                .get("label")
                .and_then(Value::as_str)
                .map(|s| s == target)
                .unwrap_or(false)
                || map
                    .get("labels")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().any(|v| v.as_str() == Some(target)))
                    .unwrap_or(false);
            if has_label {
                let kind = map
                    .get("kind")
                    .and_then(Value::as_str)
                    .or_else(|| map.get("tag").and_then(Value::as_str))
                    .unwrap_or("unknown")
                    .to_string();
                let id = map
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| map.get("xml:id").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| target.to_string());
                let char_start = map
                    .get("char_start")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize);
                let location = match char_start {
                    Some(off) => format!("char_offset:{off}"),
                    None => path.join("/"),
                };
                *found = Some(json!({
                    "kind": kind,
                    "id": id,
                    "location": location,
                }));
                return;
            }
            // Recurse.
            for (k, v) in map {
                path.push(k.clone());
                search_label_in_ast(target, v, path, found);
                path.pop();
                if found.is_some() {
                    return;
                }
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                path.push(format!("[{i}]"));
                search_label_in_ast(target, v, path, found);
                path.pop();
                if found.is_some() {
                    return;
                }
            }
        }
        _ => {}
    }
}

/// Search markdown for `\label{TARGET}`. We classify by looking at the
/// enclosing context: if we're inside a `\begin{theorem}...\end{theorem}` (or
/// similar), kind=theorem; if there's an `\begin{equation}` nearby, kind=equation;
/// otherwise fall back to the section the label sits in.
fn resolve_label_in_markdown(label: &str, body: &str, ctx: &ToolCtx<'_>) -> Option<Value> {
    let needle = format!("\\label{{{label}}}");
    let offset = body.find(&needle)?;

    // Inspect a window before the label to figure out what kind of block we
    // are in.
    let window_start = offset.saturating_sub(2_000);
    let window = &body[window_start..offset];

    // Find the most recent `\begin{...}` before the label that hasn't been
    // closed by an `\end{...}`.
    let begin_re = Regex::new(r"\\begin\{([a-zA-Z*]+)\}").ok()?;
    let end_re = Regex::new(r"\\end\{([a-zA-Z*]+)\}").ok()?;
    let mut stack: Vec<&str> = Vec::new();
    let mut events: Vec<(usize, bool, &str)> = Vec::new();
    for c in begin_re.captures_iter(window) {
        let full = c.get(0).unwrap();
        let name = c.get(1).unwrap().as_str();
        events.push((full.start(), true, name));
    }
    for c in end_re.captures_iter(window) {
        let full = c.get(0).unwrap();
        let name = c.get(1).unwrap().as_str();
        events.push((full.start(), false, name));
    }
    events.sort_by_key(|e| e.0);
    for (_pos, is_begin, name) in events {
        if is_begin {
            stack.push(name);
        } else if let Some(top) = stack.last() {
            if top.trim_end_matches('*') == name.trim_end_matches('*') {
                stack.pop();
            }
        }
    }

    let kind = match stack.last() {
        Some(env) => match env.trim_end_matches('*') {
            "theorem" => "theorem",
            "lemma" => "lemma",
            "proposition" => "proposition",
            "corollary" => "corollary",
            "definition" => "definition",
            "proof" => "proof",
            "remark" => "remark",
            "construction" => "construction",
            "example" => "example",
            "equation" | "align" | "gather" | "multline" | "eqnarray" => "equation",
            _ => "section",
        },
        None => "section",
    };

    // If the label looks like `eq:...` and we're not in an environment, still
    // call it an equation.
    let kind = if kind == "section" && label.starts_with("eq:") {
        "equation"
    } else {
        kind
    };

    // Find the section that contains this offset.
    let sections = sections_from_markdown(body);
    let containing = sections
        .iter()
        .find(|s| s.char_start <= offset && offset < s.char_end)
        .map(|s| s.id.clone());
    let location = match containing {
        Some(id) => format!("{}@char_offset:{}", id, offset),
        None => format!("char_offset:{offset}"),
    };

    let _ = ctx; // ctx kept for future use (line numbers, etc.)
    Some(json!({
        "kind": kind,
        "id": label,
        "location": location,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::ToolCtx;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grokrxiv-theorems-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn ctx_with_ast<'a>(workdir: &'a std::path::Path, ast: &'a Value) -> ToolCtx<'a> {
        ToolCtx {
            workdir,
            semantic_ast: Some(ast),
            source_id: "test",
            http: Arc::new(reqwest::Client::new()),
        }
    }
    fn ctx_no_ast(workdir: &std::path::Path) -> ToolCtx<'_> {
        ToolCtx {
            workdir,
            semantic_ast: None,
            source_id: "test",
            http: Arc::new(reqwest::Client::new()),
        }
    }

    #[test]
    fn list_sections_from_ast() {
        let ast = json!({
            "kind": "document",
            "children": [
                {"kind": "section", "title": "Intro", "char_start": 0, "char_end": 100, "children": [
                    {"kind": "subsection", "title": "Motivation", "char_start": 10, "char_end": 60}
                ]},
                {"kind": "section", "title": "Main", "char_start": 100, "char_end": 500}
            ]
        });
        let dir = tempdir();
        let ctx = ctx_with_ast(dir.path(), &ast);
        let out = list_sections(&ctx).unwrap();
        assert_eq!(out.len(), 3, "expected 3 entries, got {out:?}");
        assert_eq!(out[0]["heading"], "Intro");
        assert_eq!(out[0]["level"], 1);
        assert_eq!(out[1]["heading"], "Motivation");
        assert_eq!(out[1]["level"], 2);
        assert_eq!(out[2]["heading"], "Main");
        assert_eq!(out[2]["level"], 1);
    }

    #[test]
    fn list_sections_fallback_to_markdown() {
        let dir = tempdir();
        let body = "# Title\n\nintro text\n\n## Intro\n\nfoo\n\n## Main\n\nbar\n\n### Sub\n\nbaz\n";
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let out = list_sections(&ctx).unwrap();
        assert_eq!(out.len(), 4, "expected 4 entries, got {out:?}");
        assert_eq!(out[0]["heading"], "Title");
        assert_eq!(out[0]["level"], 1);
        assert_eq!(out[1]["heading"], "Intro");
        assert_eq!(out[1]["level"], 2);
        assert_eq!(out[2]["heading"], "Main");
        assert_eq!(out[2]["level"], 2);
        assert_eq!(out[3]["heading"], "Sub");
        assert_eq!(out[3]["level"], 3);
    }

    #[test]
    fn read_section_extracts_theorem_block() {
        let dir = tempdir();
        let body = "# Section\n\nintro\n\n\\begin{theorem}\\label{thm:foo} Let X be a Hausdorff space. \\end{theorem}\n\nmore text\n";
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let sec_list = list_sections(&ctx).unwrap();
        let id = sec_list[0]["id"].as_str().unwrap();
        let out = read_section(id, &ctx).unwrap();
        let theorems = out["theorems"].as_array().unwrap();
        assert_eq!(theorems.len(), 1, "expected 1 theorem, got {theorems:?}");
        assert_eq!(theorems[0]["label"], "thm:foo");
        assert_eq!(theorems[0]["type"], "theorem");
        assert!(theorems[0]["statement_preview"]
            .as_str()
            .unwrap()
            .contains("Hausdorff"));
    }

    #[test]
    fn read_section_returns_full_theorem_source_not_only_preview() {
        let dir = tempdir();
        let long_statement = format!(
            "Let n be a natural number. {} Therefore n + 0 = n.",
            "Assume the canonical recursive definition of addition. ".repeat(8)
        );
        let tex =
            format!("\\begin{{theorem}}\\label{{thm:add-zero}} {long_statement} \\end{{theorem}}");
        let body = format!("# Section\n\n{tex}\n");
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let sec_list = list_sections(&ctx).unwrap();
        let id = sec_list[0]["id"].as_str().unwrap();
        let out = read_section(id, &ctx).unwrap();
        let theorem = &out["theorems"].as_array().unwrap()[0];

        assert!(theorem["statement_preview"]
            .as_str()
            .unwrap()
            .ends_with("..."));
        assert!(theorem["statement"]
            .as_str()
            .unwrap()
            .contains("Therefore n + 0 = n."));
        assert!(!theorem["statement"].as_str().unwrap().ends_with("..."));
        assert_eq!(theorem["source_tex"], tex);
    }

    #[test]
    fn read_section_extracts_construction_block() {
        let dir = tempdir();
        let body = "# Section\n\n\\begin{construction}\\label{constr:frame} Choose an adapted frame. \\end{construction}\n";
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let sec_list = list_sections(&ctx).unwrap();
        let id = sec_list[0]["id"].as_str().unwrap();
        let out = read_section(id, &ctx).unwrap();
        let theorems = out["theorems"].as_array().unwrap();
        assert_eq!(theorems.len(), 1, "expected 1 block, got {theorems:?}");
        assert_eq!(theorems[0]["label"], "constr:frame");
        assert_eq!(theorems[0]["type"], "construction");
    }

    #[test]
    fn read_section_handles_markdown_theorem() {
        let dir = tempdir();
        let body =
            "# Section\n\n**Theorem 2.1.** Let X be Hausdorff and compact.\n\nProof follows.\n";
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let sec_list = list_sections(&ctx).unwrap();
        let id = sec_list[0]["id"].as_str().unwrap();
        let out = read_section(id, &ctx).unwrap();
        let theorems = out["theorems"].as_array().unwrap();
        assert!(
            theorems.iter().any(|t| t["type"] == "theorem"),
            "expected a theorem entry, got {theorems:?}"
        );
        let first = theorems.iter().find(|t| t["type"] == "theorem").unwrap();
        assert!(first["statement_preview"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("hausdorff"));
    }

    #[test]
    fn resolve_label_finds_theorem() {
        let ast = json!({
            "kind": "document",
            "children": [
                {"kind": "section", "title": "Main", "char_start": 0, "char_end": 200, "children": [
                    {"kind": "theorem", "id": "T1", "label": "thm:foo", "char_start": 50}
                ]}
            ]
        });
        let dir = tempdir();
        let ctx = ctx_with_ast(dir.path(), &ast);
        let out = resolve_label("thm:foo", &ctx).unwrap();
        assert_eq!(out["kind"], "theorem");
        assert_eq!(out["id"], "T1");
        assert!(out["location"].as_str().unwrap().contains("50"));
    }

    #[test]
    fn resolve_label_unknown_returns_unknown_kind() {
        let dir = tempdir();
        let ctx = ctx_no_ast(dir.path());
        let out = resolve_label("thm:does_not_exist", &ctx).unwrap();
        assert_eq!(out["kind"], "unknown");
        assert_eq!(out["id"], "thm:does_not_exist");
        assert_eq!(out["location"], "");
    }

    #[test]
    fn resolve_label_finds_construction_environment() {
        let dir = tempdir();
        let body = "# Section\n\n\\begin{construction}\\label{constr:frame} Choose an adapted frame. \\end{construction}\n";
        std::fs::write(dir.path().join("body.md"), body).unwrap();
        let ctx = ctx_no_ast(dir.path());
        let out = resolve_label("constr:frame", &ctx).unwrap();
        assert_eq!(out["kind"], "construction");
        assert_eq!(out["id"], "constr:frame");
        assert!(out["location"].as_str().unwrap().contains("char_offset"));
    }
}
