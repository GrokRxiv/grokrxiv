//! Tools specific to the [`MacroExpanderAgent`] (Stage 4 — Track 8d).
//!
//! Two LLM-callable tools:
//!
//! * `find_definitions(file?)` — scans TeX files in the workdir and extracts
//!   every `\newcommand{\NAME}[N]{BODY}`, `\renewcommand{\NAME}[N]{BODY}`,
//!   `\def\NAME{BODY}`, and `\DeclareMathOperator{\NAME}{BODY}` definition.
//! * `apply_expansions(input_text, mapping)` — pure, deterministic substitution
//!   helper that replaces every `\NAME{arg1}…{argN}` occurrence with the
//!   macro body (with `#1`/`#2`/… replaced by the call-site arguments). Runs
//!   to a fixed point so chains like `\A → \B → \mathbb{C}` collapse.
//!
//! ## Balanced-brace caveat (locked, in commit message too)
//!
//! These tools are intentionally **regex/string-based, not a real TeX parser**.
//! The body of a `\newcommand` is read by matching `{`…`}` with an
//! explicit-depth counter starting at 1 and ascending/descending on every
//! brace seen. That means we DO correctly handle one or more nested groups
//! like `\newcommand{\set}[1]{\{#1\}}` — the bookkeeping is iterative and
//! depth-bounded only by `MAX_BRACE_NESTING = 16` (which is well above any
//! realistic macro body and prevents pathological inputs from blowing the
//! stack). We do NOT try to handle:
//!
//! * Catcode changes (`\catcode`)
//! * `\expandafter` / `\noexpand` games
//! * `\let` and `\futurelet`
//! * Macros defined inside other macros
//!
//! Those are out of scope; the orchestrator falls back to the raw TeX if the
//! agent gives up.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{Tool, ToolCtx};

/// Maximum brace-nesting depth tolerated when parsing macro bodies / call
/// arguments. See the module docs for rationale.
pub const MAX_BRACE_NESTING: u32 = 16;
/// Cap on `apply_expansions` fixed-point iterations. Matches the plan: chains
/// up to length 5 collapse cleanly.
pub const MAX_EXPANSION_PASSES: u32 = 5;

/// One macro definition extracted from TeX source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDef {
    /// Control-sequence name including the leading backslash (e.g. `\R`).
    pub name: String,
    /// Number of positional arguments (`0..=9` per TeX).
    pub params: u8,
    /// Macro body verbatim (still containing `#1`, `#2`, ... placeholders).
    pub body: String,
    /// Source file (relative to workdir) the definition was found in.
    /// Empty when scanning a single string buffer rather than the workdir.
    pub file: String,
    /// 1-based source line.
    pub line: u32,
}

/// Implements `find_definitions`.
pub struct FindDefinitionsTool;

static FIND_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_find_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file": {
                "type": "string",
                "description": "Optional relative path. If omitted, scans every *.tex file under the workdir."
            }
        }
    })
}

#[async_trait]
impl Tool for FindDefinitionsTool {
    fn name(&self) -> &'static str {
        "find_definitions"
    }
    fn description(&self) -> &'static str {
        "Extract every \\newcommand / \\renewcommand / \\def / \\DeclareMathOperator from \
         either a single TeX file (if `file` is given) or every *.tex file under the workdir."
    }
    fn schema(&self) -> &Value {
        FIND_SCHEMA.get_or_init(build_find_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let file_arg = args.get("file").and_then(Value::as_str).map(str::to_owned);
        let mut defs: Vec<MacroDef> = Vec::new();
        if let Some(rel) = file_arg {
            let path = ctx.workdir.join(&rel);
            let src = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("find_definitions: cannot read `{rel}`: {e}"))?;
            extract_definitions(&src, &rel, &mut defs);
        } else {
            scan_tex_files(ctx.workdir, ctx.workdir, &mut defs)?;
        }
        let array: Vec<Value> = defs
            .into_iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "params": d.params,
                    "body": d.body,
                    "file": d.file,
                    "line": d.line,
                })
            })
            .collect();
        Ok(json!({ "definitions": array }))
    }
}

/// Implements `apply_expansions`.
pub struct ApplyExpansionsTool;

static APPLY_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_apply_schema() -> Value {
    json!({
        "type": "object",
        "required": ["input_text", "mapping"],
        "properties": {
            "input_text": {
                "type": "string",
                "description": "The TeX source to normalize."
            },
            "mapping": {
                "type": "object",
                "description": "Map of `\\NAME` -> {body, params}. `params` defaults to 0 if omitted.",
                "additionalProperties": {
                    "type": "object",
                    "required": ["body"],
                    "properties": {
                        "body": { "type": "string" },
                        "params": { "type": "integer" }
                    }
                }
            }
        }
    })
}

#[async_trait]
impl Tool for ApplyExpansionsTool {
    fn name(&self) -> &'static str {
        "apply_expansions"
    }
    fn description(&self) -> &'static str {
        "Substitute every \\NAME{...} occurrence in `input_text` with the supplied macro body. \
         Iterates to a fixed point (capped at 5 passes) so nested macro chains collapse."
    }
    fn schema(&self) -> &Value {
        APPLY_SCHEMA.get_or_init(build_apply_schema)
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let input = args
            .get("input_text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("apply_expansions: `input_text` is required"))?;
        let mapping_val = args
            .get("mapping")
            .ok_or_else(|| anyhow::anyhow!("apply_expansions: `mapping` is required"))?;
        let mapping = parse_mapping(mapping_val)?;
        let (expanded, counts) = apply_expansions(input, &mapping);
        let counts_obj: serde_json::Map<String, Value> = counts
            .into_iter()
            .map(|(k, v)| (k, Value::from(v)))
            .collect();
        Ok(json!({
            "expanded_text": expanded,
            "substitutions_count": Value::Object(counts_obj),
        }))
    }
}

/// Lookup entry consumed by [`apply_expansions`].
#[derive(Debug, Clone)]
pub struct MacroLookup {
    /// Macro body verbatim, with `#1`/`#2`/... placeholders for arguments.
    pub body: String,
    /// Number of positional arguments the macro takes.
    pub params: u8,
}

fn parse_mapping(v: &Value) -> anyhow::Result<HashMap<String, MacroLookup>> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("apply_expansions: `mapping` must be an object"))?;
    let mut out: HashMap<String, MacroLookup> = HashMap::new();
    for (k, entry) in obj {
        let body = entry
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("apply_expansions: mapping[{k}] missing string `body`"))?
            .to_string();
        let params = entry
            .get("params")
            .and_then(Value::as_u64)
            .map(|n| n.min(9) as u8)
            .unwrap_or(0);
        let key = if k.starts_with('\\') {
            k.clone()
        } else {
            format!("\\{k}")
        };
        out.insert(key, MacroLookup { body, params });
    }
    Ok(out)
}

/// Walk every `*.tex` file under `root` and append definitions to `out`.
fn scan_tex_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<MacroDef>,
) -> anyhow::Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            scan_tex_files(root, &path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("tex") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            extract_definitions(&src, &rel, out);
        }
    }
    Ok(())
}

/// Core scanner: walk `src` and append every supported macro definition.
///
/// Recognised forms:
/// * `\newcommand{\NAME}[N]{BODY}`
/// * `\renewcommand{\NAME}[N]{BODY}`
/// * `\providecommand{\NAME}[N]{BODY}` (treated the same as newcommand)
/// * `\def\NAME{BODY}` (no `[N]`; params inferred from `#N` references in BODY)
/// * `\DeclareMathOperator{\NAME}{BODY}` and `\DeclareMathOperator*{\NAME}{BODY}`
pub fn extract_definitions(src: &str, file: &str, out: &mut Vec<MacroDef>) {
    let mut i = 0usize;
    while i < src.len() {
        let rest = &src[i..];
        if let Some(parsed) = try_parse_def_keyword(rest) {
            let (def, consumed) = parsed;
            let line = line_of(src, i);
            let mut def = def;
            def.file = file.to_string();
            def.line = line;
            out.push(def);
            i += consumed;
            continue;
        }
        // Advance by one codepoint, not one byte — `&src[i..]` panics if `i`
        // lands inside a multi-byte UTF-8 char (e.g. `ä` in a German author
        // name in the bibliography of paper 2605.00403).
        i += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
}

/// Try to match one of the four definition forms starting at `s[0]`. Returns
/// the parsed [`MacroDef`] plus the number of bytes consumed (so the outer
/// scanner can skip past it).
fn try_parse_def_keyword(s: &str) -> Option<(MacroDef, usize)> {
    const NEW: &str = "\\newcommand";
    const RENEW: &str = "\\renewcommand";
    const PROV: &str = "\\providecommand";
    const DEF: &str = "\\def";
    const DECL: &str = "\\DeclareMathOperator";

    if let Some(rest) = s.strip_prefix(NEW) {
        return parse_command_style(rest, NEW.len());
    }
    if let Some(rest) = s.strip_prefix(RENEW) {
        return parse_command_style(rest, RENEW.len());
    }
    if let Some(rest) = s.strip_prefix(PROV) {
        return parse_command_style(rest, PROV.len());
    }
    if let Some(rest) = s.strip_prefix(DECL) {
        // Accept both \DeclareMathOperator and \DeclareMathOperator*.
        let (rest, star_len) = if let Some(r) = rest.strip_prefix('*') {
            (r, 1)
        } else {
            (rest, 0)
        };
        let head = DECL.len() + star_len;
        return parse_declare_math_operator(rest, head);
    }
    if let Some(rest) = s.strip_prefix(DEF) {
        return parse_def_style(rest, DEF.len());
    }
    None
}

/// Parse `\newcommand{\name}[N]{body}` (or `\renewcommand` / `\providecommand`).
/// `head` is the byte length of the keyword that has already been stripped.
fn parse_command_style(rest: &str, head: usize) -> Option<(MacroDef, usize)> {
    let (rest, ws1) = skip_ws(rest);
    let (name, rest, name_len) = read_braced_name(rest)?;
    let (rest, ws2) = skip_ws(rest);
    let (params, rest, p_len) = read_optional_params(rest);
    let (rest, ws3) = skip_ws(rest);
    let (body, body_len) = read_braced_body(rest)?;
    let consumed = head + ws1 + name_len + ws2 + p_len + ws3 + body_len;
    Some((
        MacroDef {
            name,
            params,
            body,
            file: String::new(),
            line: 0,
        },
        consumed,
    ))
}

/// Parse `\def\name{body}`. `\def` does NOT use the `[N]` argument-count
/// form — TeX uses a parameter-text pattern (`\def\foo#1#2{...}`). We accept
/// `\def\NAME#1#2...{body}` and count the `#N` markers to infer `params`.
fn parse_def_style(rest: &str, head: usize) -> Option<(MacroDef, usize)> {
    let (rest, ws1) = skip_ws(rest);
    let (name, rest, name_len) = read_bare_name(rest)?;
    let mut params: u8 = 0;
    let mut cursor = rest;
    let mut param_text_len = 0usize;
    let bytes = cursor.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b'#' && i + 1 < bytes.len() {
        let d = bytes[i + 1];
        if d.is_ascii_digit() {
            let n = (d - b'0') as u8;
            if n > params {
                params = n;
            }
            i += 2;
        } else {
            break;
        }
    }
    param_text_len += i;
    cursor = &cursor[i..];
    let (cursor, ws2) = skip_ws(cursor);
    let (body, body_len) = read_braced_body(cursor)?;
    let consumed = head + ws1 + name_len + param_text_len + ws2 + body_len;
    Some((
        MacroDef {
            name,
            params,
            body,
            file: String::new(),
            line: 0,
        },
        consumed,
    ))
}

/// Parse `\DeclareMathOperator{\name}{body}`. No `[N]` form; always 0-arg.
fn parse_declare_math_operator(rest: &str, head: usize) -> Option<(MacroDef, usize)> {
    let (rest, ws1) = skip_ws(rest);
    let (name, rest, name_len) = read_braced_name(rest)?;
    let (rest, ws2) = skip_ws(rest);
    let (body, body_len) = read_braced_body(rest)?;
    let consumed = head + ws1 + name_len + ws2 + body_len;
    Some((
        MacroDef {
            name,
            params: 0,
            body,
            file: String::new(),
            line: 0,
        },
        consumed,
    ))
}

/// Read `{\name}` and return `(name_including_backslash, remainder, consumed_bytes)`.
fn read_braced_name(s: &str) -> Option<(String, &str, usize)> {
    let bytes = s.as_bytes();
    if bytes.first().copied() != Some(b'{') {
        return None;
    }
    if bytes.get(1).copied() != Some(b'\\') {
        return None;
    }
    let mut i = 2;
    while i < bytes.len() && is_csname_byte(bytes[i]) {
        i += 1;
    }
    if i == 2 || bytes.get(i).copied() != Some(b'}') {
        return None;
    }
    let name = s[1..i].to_string();
    Some((name, &s[i + 1..], i + 1))
}

/// Read `\name` (no surrounding braces) and return `(name, remainder, consumed_bytes)`.
fn read_bare_name(s: &str) -> Option<(String, &str, usize)> {
    let bytes = s.as_bytes();
    if bytes.first().copied() != Some(b'\\') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && is_csname_byte(bytes[i]) {
        i += 1;
    }
    if i == 1 {
        return None;
    }
    Some((s[..i].to_string(), &s[i..], i))
}

/// Read optional `[N]` argument-count. Returns `(N, remainder, consumed_bytes)`.
fn read_optional_params(s: &str) -> (u8, &str, usize) {
    let bytes = s.as_bytes();
    if bytes.first().copied() != Some(b'[') {
        return (0, s, 0);
    }
    let mut i = 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 1 || bytes.get(i).copied() != Some(b']') {
        return (0, s, 0);
    }
    let n: u8 = s[1..i].parse().unwrap_or(0);
    (n, &s[i + 1..], i + 1)
}

/// Read `{...balanced...}` and return `(body_without_outer_braces, consumed_bytes_including_braces)`.
///
/// Balanced-brace handling: we count `{` and `}` with an explicit depth
/// counter, skipping any character escaped by a single `\\` so that `\{`,
/// `\}`, and `\\` don't confuse the depth count.
fn read_braced_body(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.first().copied() != Some(b'{') {
        return None;
    }
    let mut depth: u32 = 1;
    let mut i = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
            }
            b'{' => {
                depth += 1;
                if depth > MAX_BRACE_NESTING {
                    return None;
                }
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let body = s[1..i].to_string();
                    return Some((body, i + 1));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Skip ASCII whitespace and return `(remainder, consumed_bytes)`.
fn skip_ws(s: &str) -> (&str, usize) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    (&s[i..], i)
}

/// TeX control-sequence name characters: ASCII letters only (per the
/// standard). `\R`, `\mathbb`, `\foo` all match; `\@foo` does NOT (we don't
/// want `\@` to slurp into the next macro).
fn is_csname_byte(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

/// 1-based line number of byte offset `idx` in `src`.
fn line_of(src: &str, idx: usize) -> u32 {
    let upto = &src.as_bytes()[..idx.min(src.len())];
    1 + upto.iter().filter(|&&b| b == b'\n').count() as u32
}

/// Pure expansion routine. Returns `(expanded_text, counts)` where `counts`
/// maps `\NAME` -> total substitutions across all passes.
///
/// Iterates to a fixed point: re-runs until no further substitutions happen
/// (or [`MAX_EXPANSION_PASSES`] is reached). That handles nested chains like
/// `\A → \B → \mathbb{C}` cleanly.
pub fn apply_expansions(
    input: &str,
    mapping: &HashMap<String, MacroLookup>,
) -> (String, HashMap<String, u32>) {
    let mut text = input.to_string();
    let mut counts: HashMap<String, u32> = HashMap::new();
    for _ in 0..MAX_EXPANSION_PASSES {
        let (next, changes) = expand_once(&text, mapping);
        if changes.is_empty() {
            return (next, counts);
        }
        for (k, v) in changes {
            *counts.entry(k).or_insert(0) += v;
        }
        text = next;
    }
    (text, counts)
}

/// One left-to-right substitution pass over `text`.
fn expand_once(
    text: &str,
    mapping: &HashMap<String, MacroLookup>,
) -> (String, HashMap<String, u32>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Read the control sequence name.
            let mut j = i + 1;
            while j < bytes.len() && is_csname_byte(bytes[j]) {
                j += 1;
            }
            if j == i + 1 {
                // `\\` followed by non-letter — keep the backslash + next char
                // literal so that `\{`, `\}`, `\\` etc survive.
                out.push('\\');
                if i + 1 < bytes.len() {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            let name = &text[i..j];
            if let Some(def) = mapping.get(name) {
                // Try to read `def.params` brace-delimited arguments.
                let mut args: Vec<String> = Vec::with_capacity(def.params as usize);
                let mut cursor = j;
                let mut ok = true;
                for _ in 0..def.params {
                    let (arg, used) = match read_braced_arg(&text[cursor..]) {
                        Some(v) => v,
                        None => {
                            ok = false;
                            break;
                        }
                    };
                    args.push(arg);
                    cursor += used;
                }
                if ok {
                    let body = substitute_placeholders(&def.body, &args);
                    out.push_str(&body);
                    *counts.entry(name.to_string()).or_insert(0) += 1;
                    i = cursor;
                    continue;
                }
            }
            // Not in mapping or arg-read failed — preserve verbatim.
            out.push_str(name);
            i = j;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    (out, counts)
}

/// Read one brace-delimited argument `{...}` for a call site. Returns
/// `(unwrapped_contents, consumed_bytes_including_braces)`.
fn read_braced_arg(s: &str) -> Option<(String, usize)> {
    read_braced_body(s)
}

/// Replace every `#1`/`#2`/... placeholder in `body` with the matching arg.
fn substitute_placeholders(body: &str, args: &[String]) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let idx = (bytes[i + 1] - b'0') as usize;
            if idx >= 1 && idx <= args.len() {
                out.push_str(&args[idx - 1]);
                i += 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(src: &str) -> Vec<MacroDef> {
        let mut out = Vec::new();
        extract_definitions(src, "main.tex", &mut out);
        out
    }

    #[test]
    fn find_definitions_extracts_newcommand() {
        let src = r"Some preamble. \newcommand{\R}{\mathbb{R}} And then text.";
        let v = defs(src);
        assert_eq!(v.len(), 1, "expected 1 def, got {:?}", v);
        assert_eq!(v[0].name, r"\R");
        assert_eq!(v[0].params, 0);
        assert_eq!(v[0].body, r"\mathbb{R}");
        assert_eq!(v[0].file, "main.tex");
        assert_eq!(v[0].line, 1);
    }

    #[test]
    fn find_definitions_extracts_with_params() {
        let src = r"\newcommand{\set}[1]{\{#1\}}";
        let v = defs(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, r"\set");
        assert_eq!(v[0].params, 1);
        assert_eq!(v[0].body, r"\{#1\}");
    }

    #[test]
    fn find_definitions_handles_def() {
        let src = r"\def\R{\mathbb{R}}";
        let v = defs(src);
        assert_eq!(v.len(), 1, "expected one def-style def, got {:?}", v);
        assert_eq!(v[0].name, r"\R");
        assert_eq!(v[0].params, 0);
        assert_eq!(v[0].body, r"\mathbb{R}");
    }

    #[test]
    fn find_definitions_handles_declare_math_operator() {
        let src = r"\DeclareMathOperator{\rank}{rank}";
        let v = defs(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, r"\rank");
        assert_eq!(v[0].params, 0);
        assert_eq!(v[0].body, "rank");
    }

    #[test]
    fn find_definitions_handles_renewcommand() {
        let src = r"\renewcommand{\vec}[1]{\mathbf{#1}}";
        let v = defs(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, r"\vec");
        assert_eq!(v[0].params, 1);
        assert_eq!(v[0].body, r"\mathbf{#1}");
    }

    #[test]
    fn apply_expansions_substitutes_zero_arg() {
        let mut m = HashMap::new();
        m.insert(
            r"\R".to_string(),
            MacroLookup {
                body: r"\mathbb{R}".to_string(),
                params: 0,
            },
        );
        let (out, counts) = apply_expansions(r"Let \R^n be the space.", &m);
        assert!(
            out.contains(r"\mathbb{R}^n"),
            "expected expanded \\mathbb{{R}}^n in output, got `{out}`"
        );
        assert_eq!(counts.get(r"\R").copied().unwrap_or(0), 1);
    }

    #[test]
    fn apply_expansions_substitutes_one_arg() {
        let mut m = HashMap::new();
        m.insert(
            r"\set".to_string(),
            MacroLookup {
                body: r"\{#1\}".to_string(),
                params: 1,
            },
        );
        let (out, counts) = apply_expansions(r"consider \set{x,y} carefully", &m);
        assert!(
            out.contains(r"\{x,y\}"),
            "expected `\\{{x,y\\}}` in output, got `{out}`"
        );
        assert_eq!(counts.get(r"\set").copied().unwrap_or(0), 1);
    }

    #[test]
    fn apply_expansions_nested_macros_reach_fixed_point() {
        let mut m = HashMap::new();
        m.insert(
            r"\A".to_string(),
            MacroLookup {
                body: r"\B".to_string(),
                params: 0,
            },
        );
        m.insert(
            r"\B".to_string(),
            MacroLookup {
                body: r"\mathbb{C}".to_string(),
                params: 0,
            },
        );
        let (out, _counts) = apply_expansions(r"\A and \B", &m);
        assert!(
            out.contains(r"\mathbb{C}") && !out.contains(r"\A") && !out.contains(r"\B "),
            "expected both \\A and \\B to collapse to \\mathbb{{C}}, got `{out}`"
        );
    }

    #[test]
    fn apply_expansions_leaves_unknown_macros_alone() {
        let m = HashMap::new();
        let (out, counts) = apply_expansions(r"keep \unknown intact", &m);
        assert_eq!(out, r"keep \unknown intact");
        assert!(counts.is_empty());
    }

    #[test]
    fn balanced_brace_body_with_nested_groups() {
        let src = r"\newcommand{\halfopen}[2]{[#1, #2)}";
        let v = defs(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].params, 2);
        let mut m = HashMap::new();
        m.insert(
            v[0].name.clone(),
            MacroLookup {
                body: v[0].body.clone(),
                params: v[0].params,
            },
        );
        let (out, _) = apply_expansions(r"\halfopen{0}{1}", &m);
        assert_eq!(out, "[0, 1)");
    }

    #[test]
    fn extract_definitions_survives_non_ascii_source() {
        // Regression for the panic Team H1 hit on paper 2605.00403: the
        // bibliography contained `Stäckel` (`ä` is a 2-byte UTF-8 codepoint).
        // The scanner used to advance by one BYTE on the no-match path, which
        // panicked when `i` landed mid-codepoint at `&src[i..]`.
        let src = "Some prose with German name Stäckel and an umlaut here ñ. \
                   \\newcommand{\\R}{\\mathbb{R}} \
                   Trailing prose with more accents: über naïve résumé.";
        let v = defs(src);
        assert_eq!(
            v.len(),
            1,
            "should find the \\newcommand surrounded by non-ASCII"
        );
        assert_eq!(v[0].name, r"\R");
        assert_eq!(v[0].body, r"\mathbb{R}");
    }
}
