//! Per-agent tools for the [`EquationCanonicalizerAgent`].
//!
//! Three tools live here:
//!
//! - `list_equations` — walks `ctx.semantic_ast` (preferred) or falls back to
//!   scanning the unpacked workdir's `body.md` for `\(...\)` / `\[...\]` math.
//! - `render_to_mathml` — shells out to `latexml` and pulls the `<math>`
//!   element from the result. Wrapped in a 30 s timeout; if `latexml` isn't on
//!   PATH the tool reports `ok=false` rather than failing the loop.
//! - `equation_hash` — a deliberately FUZZY SHA-256 over normalised TeX used
//!   for dedup (not a proof of mathematical equivalence — see
//!   [`equation_hash::canonicalise`]).

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};

use crate::agents::extraction::{Tool, ToolCtx};

// ---------------------------------------------------------------------------
// list_equations
// ---------------------------------------------------------------------------

/// `list_equations()` — every equation in the paper as `{id, tex, context, kind}`.
pub struct ListEquationsTool;

static LIST_EQUATIONS_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn list_equations_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "description": "No arguments. Returns every equation in the paper."
    })
}

#[async_trait]
impl Tool for ListEquationsTool {
    fn name(&self) -> &'static str {
        "list_equations"
    }
    fn description(&self) -> &'static str {
        "List every equation in the paper as {id, tex, context, kind}. Walks the \
         semantic_ast when available; otherwise falls back to scanning body.md."
    }
    fn schema(&self) -> &Value {
        LIST_EQUATIONS_SCHEMA.get_or_init(list_equations_schema)
    }
    async fn call(&self, _args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let equations = match ctx.semantic_ast {
            Some(ast) => list_from_ast(ast),
            None => list_from_markdown(ctx.workdir),
        };
        Ok(json!({ "equations": equations }))
    }
}

/// Walk a semantic_ast JSON value collecting every math/equation/align/etc
/// node. The output entries each carry `id`, `tex`, `context` (nearest
/// preceding section heading) and `kind` (`inline` | `display`).
pub fn list_from_ast(root: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut counter = 0usize;
    let mut current_section = String::new();
    collect_ast(root, &mut current_section, &mut counter, &mut out);
    out
}

fn collect_ast(
    v: &Value,
    current_section: &mut String,
    counter: &mut usize,
    out: &mut Vec<Value>,
) {
    match v {
        Value::Object(map) => {
            // Heading nodes: capture for the "context" field on following math nodes.
            // We accept either `{"type":"heading","text":"..."}` or
            // `{"type":"section","title":"..."}` shapes.
            if let Some(kind) = map.get("type").and_then(Value::as_str) {
                if kind == "heading" || kind == "title" {
                    if let Some(t) = map.get("text").and_then(Value::as_str) {
                        *current_section = t.to_string();
                    }
                } else if kind == "section" {
                    if let Some(t) = map.get("title").and_then(Value::as_str) {
                        *current_section = t.to_string();
                    }
                }
                if is_math_node_kind(kind) {
                    *counter += 1;
                    let id = map
                        .get("xml:id")
                        .or_else(|| map.get("id"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("eq-{}", *counter));
                    let tex = extract_tex(map);
                    let math_kind = ast_kind_to_math_kind(kind);
                    out.push(json!({
                        "id": id,
                        "tex": tex,
                        "context": current_section.clone(),
                        "kind": math_kind,
                    }));
                    return;
                }
            }
            for (_k, child) in map {
                collect_ast(child, current_section, counter, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                collect_ast(child, current_section, counter, out);
            }
        }
        _ => {}
    }
}

fn is_math_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "math"
            | "MathTok"
            | "equation"
            | "equation*"
            | "align"
            | "align*"
            | "displaymath"
            | "inline-math"
            | "display-math"
            | "math_block"
            | "math_inline"
    )
}

fn ast_kind_to_math_kind(kind: &str) -> &'static str {
    match kind {
        "math" | "MathTok" | "inline-math" | "math_inline" => "inline",
        _ => "display",
    }
}

fn extract_tex(map: &serde_json::Map<String, Value>) -> String {
    if let Some(t) = map.get("tex").and_then(Value::as_str) {
        return t.to_string();
    }
    if let Some(t) = map.get("text").and_then(Value::as_str) {
        return t.to_string();
    }
    if let Some(t) = map.get("content").and_then(Value::as_str) {
        return t.to_string();
    }
    if let Some(arr) = map.get("content").and_then(Value::as_array) {
        let mut buf = String::new();
        for v in arr {
            if let Some(s) = v.as_str() {
                buf.push_str(s);
            }
        }
        return buf;
    }
    String::new()
}

/// Fallback: scan `<workdir>/body.md` for `\(...\)` (inline) and `\[...\]`
/// (display) math runs. Used when `ctx.semantic_ast` is `None` because the
/// paper came in as PDF-only.
pub fn list_from_markdown(workdir: &std::path::Path) -> Vec<Value> {
    let body_path = workdir.join("body.md");
    let body = match std::fs::read_to_string(&body_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut counter = 0usize;
    let mut current_section = String::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Track section headings ("# ", "## ", etc.).
        if (i == 0 || bytes[i - 1] == b'\n') && bytes[i] == b'#' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b' ' {
                let line_end = bytes[j..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map(|p| j + p)
                    .unwrap_or(bytes.len());
                current_section = String::from_utf8_lossy(&bytes[j + 1..line_end])
                    .trim()
                    .to_string();
                i = line_end;
                continue;
            }
        }
        // Math openers: `\(` inline, `\[` display.
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let opener = bytes[i + 1];
            let (close_pair, kind): (&[u8], &str) = match opener {
                b'(' => (b"\\)", "inline"),
                b'[' => (b"\\]", "display"),
                _ => {
                    i += 1;
                    continue;
                }
            };
            let start = i + 2;
            if let Some(rel_end) = find_subslice(&bytes[start..], close_pair) {
                let tex_bytes = &bytes[start..start + rel_end];
                let tex = String::from_utf8_lossy(tex_bytes).trim().to_string();
                counter += 1;
                out.push(json!({
                    "id": format!("eq-{}", counter),
                    "tex": tex,
                    "context": current_section.clone(),
                    "kind": kind,
                }));
                i = start + rel_end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    'outer: for i in 0..=haystack.len() - needle.len() {
        for j in 0..needle.len() {
            if haystack[i + j] != needle[j] {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

// ---------------------------------------------------------------------------
// render_to_mathml
// ---------------------------------------------------------------------------

/// `render_to_mathml({tex})` — convert a single TeX fragment into MathML via
/// `latexml`. The returned `{mathml, ok, warnings}` tells the LLM whether the
/// rendering succeeded — if not (binary missing, syntax error, timeout) it can
/// choose to submit without MathML rather than aborting the whole run.
pub struct RenderToMathmlTool;

static RENDER_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn render_schema() -> Value {
    json!({
        "type": "object",
        "required": ["tex"],
        "properties": {
            "tex": {
                "type": "string",
                "description": "A single equation in TeX (without surrounding math delimiters)."
            }
        }
    })
}

#[async_trait]
impl Tool for RenderToMathmlTool {
    fn name(&self) -> &'static str {
        "render_to_mathml"
    }
    fn description(&self) -> &'static str {
        "Render a TeX equation to MathML via latexml. Returns {mathml, ok, warnings}. \
         If latexml is unavailable the tool returns ok=false rather than failing."
    }
    fn schema(&self) -> &Value {
        RENDER_SCHEMA.get_or_init(render_schema)
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let tex = args
            .get("tex")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("render_to_mathml requires `tex`"))?;
        Ok(render_to_mathml(tex).await)
    }
}

/// Public for tests. Spawns `latexml` with the TeX piped on stdin, parses the
/// emitted XML for the first `<math>...</math>` block, and returns it.
pub async fn render_to_mathml(tex: &str) -> Value {
    let bin = std::env::var("GROKRXIV_LATEXML_BIN").unwrap_or_else(|_| "latexml".to_string());
    let doc = format!(
        "\\documentclass{{article}}\n\\begin{{document}}\n$ {tex} $\n\\end{{document}}\n"
    );
    let warnings: Vec<String> = Vec::new();
    match timeout(Duration::from_secs(30), run_latexml(&bin, &doc)).await {
        Ok(Ok(xml)) => {
            if let Some(math) = extract_math_element(&xml) {
                json!({
                    "mathml": math,
                    "ok": true,
                    "warnings": warnings,
                })
            } else {
                json!({
                    "mathml": "",
                    "ok": false,
                    "warnings": ["latexml produced no <math> element".to_string()],
                })
            }
        }
        Ok(Err(e)) => json!({
            "mathml": "",
            "ok": false,
            "warnings": [format!("latexml invocation failed: {e}")],
        }),
        Err(_) => json!({
            "mathml": "",
            "ok": false,
            "warnings": ["latexml timed out after 30s".to_string()],
        }),
    }
}

async fn run_latexml(bin: &str, doc: &str) -> anyhow::Result<String> {
    let mut child = tokio::process::Command::new(bin)
        .arg("--quiet")
        .arg("--strict")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn `{bin}`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(doc.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("write stdin: {e}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| anyhow::anyhow!("close stdin: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow::anyhow!("wait latexml: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        anyhow::bail!("latexml exit {}: {}", output.status, err);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pull the first `<math ...>...</math>` block out of latexml's XML output.
fn extract_math_element(xml: &str) -> Option<String> {
    let start = xml.find("<math")?;
    let end_tag = "</math>";
    let end = xml[start..].find(end_tag)? + start + end_tag.len();
    Some(xml[start..end].to_string())
}

// ---------------------------------------------------------------------------
// equation_hash
// ---------------------------------------------------------------------------

/// `equation_hash({canonical_tex})` — fuzzy SHA-256 dedup hash.
///
/// NOTE — this is a *fuzzy* hash, NOT a proof of mathematical equivalence.
/// Two equations with the same hash are very likely duplicates; two equations
/// with different hashes are NOT guaranteed to be mathematically distinct.
/// Use it only to suppress obvious dup rows in the canonical equation list.
pub struct EquationHashTool;

static HASH_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn hash_schema() -> Value {
    json!({
        "type": "object",
        "required": ["canonical_tex"],
        "properties": {
            "canonical_tex": { "type": "string" }
        }
    })
}

#[async_trait]
impl Tool for EquationHashTool {
    fn name(&self) -> &'static str {
        "equation_hash"
    }
    fn description(&self) -> &'static str {
        "Fuzzy dedup hash for a canonical TeX equation. NOT a proof of mathematical \
         equivalence — same hash means likely duplicate; different hashes don't \
         guarantee semantic distinctness."
    }
    fn schema(&self) -> &Value {
        HASH_SCHEMA.get_or_init(hash_schema)
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let tex = args
            .get("canonical_tex")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("equation_hash requires `canonical_tex`"))?;
        Ok(json!({ "hash": equation_hash(tex) }))
    }
}

/// Public for unit tests.
pub fn equation_hash(tex: &str) -> String {
    let normalised = canonicalise(tex);
    let mut h = Sha256::new();
    h.update(normalised.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..8])
}

/// Best-effort normalisation. We:
///
/// - lowercase recognised operator names (`\sin`, `\cos`, `\log`, ...);
/// - collapse all whitespace runs to a single space and trim;
/// - sort commutative-looking pairs of the form `<single token> + <single
///   token>` (and `*`) so `a+b` and `b+a` collide.
///
/// This is deliberately conservative — over-aggressive canonicalisation
/// would collapse non-equivalent equations.
fn canonicalise(tex: &str) -> String {
    let lower_ops = lowercase_operators(tex);
    let collapsed = collapse_whitespace(&lower_ops);
    sort_commutative_pairs(&collapsed)
}

fn lowercase_operators(s: &str) -> String {
    // Walk through, find every `\<letters>` macro, and lowercase those letters
    // IF it matches a known commutative-friendly operator name. (We don't
    // touch macros we don't recognise — `\Vec` and `\vec` may be different.)
    const KNOWN: &[&str] = &[
        "sin", "cos", "tan", "csc", "sec", "cot", "arcsin", "arccos", "arctan", "sinh", "cosh",
        "tanh", "log", "ln", "exp", "lim", "max", "min", "sup", "inf", "det", "ker", "dim",
        "deg", "gcd", "lcm",
    ];
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic() {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            let name = std::str::from_utf8(&bytes[start..j]).unwrap();
            let lowered = name.to_ascii_lowercase();
            if KNOWN.contains(&lowered.as_str()) {
                out.push('\\');
                out.push_str(&lowered);
            } else {
                out.push('\\');
                out.push_str(name);
            }
            i = j;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// If the trimmed input matches `X + Y` or `X * Y` for single tokens X and Y,
/// sort the operands lexicographically. This handles the cheap commutative
/// cases without dragging in a TeX parser.
fn sort_commutative_pairs(s: &str) -> String {
    for op in ['+', '*'] {
        let trimmed = s.trim();
        if let Some(idx) = trimmed.find(op) {
            let left = trimmed[..idx].trim();
            let right = trimmed[idx + 1..].trim();
            if !left.is_empty()
                && !right.is_empty()
                && !left.contains(' ')
                && !right.contains(' ')
                && !left.contains(op)
                && !right.contains(op)
            {
                let mut pair = [left, right];
                pair.sort();
                return format!("{}{}{}", pair[0], op, pair[1]);
            }
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    /// Temp-dir guard (mirrors `extraction::loop::tests::tempdir`).
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
            "grokrxiv-equations-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    #[test]
    fn list_equations_from_ast() {
        // Fixture: a tiny AST with two math nodes (one inline, one display) plus
        // a section heading to feed the `context` field.
        let ast = json!({
            "type": "document",
            "children": [
                { "type": "heading", "text": "Introduction" },
                { "type": "math", "xml:id": "eq.main", "tex": "x+y" },
                {
                    "type": "section",
                    "title": "Results",
                    "children": [
                        { "type": "equation", "tex": "\\int_0^1 f(x)\\,dx" }
                    ]
                }
            ]
        });
        let eqs = list_from_ast(&ast);
        assert_eq!(eqs.len(), 2, "got: {eqs:?}");
        assert_eq!(eqs[0]["id"], "eq.main");
        assert_eq!(eqs[0]["tex"], "x+y");
        assert_eq!(eqs[0]["kind"], "inline");
        assert_eq!(eqs[0]["context"], "Introduction");
        assert_eq!(eqs[1]["id"], "eq-2");
        assert_eq!(eqs[1]["kind"], "display");
        assert_eq!(eqs[1]["context"], "Results");
    }

    #[test]
    fn list_equations_fallback_to_markdown() {
        let tmp = tempdir();
        let body = "# Setup\n\nWe have \\(a+b\\) and then later\n\n\\[\\int_0^1 f\\]\n";
        std::fs::write(tmp.path().join("body.md"), body).unwrap();
        let eqs = list_from_markdown(tmp.path());
        assert_eq!(eqs.len(), 2, "got: {eqs:?}");
        assert_eq!(eqs[0]["tex"], "a+b");
        assert_eq!(eqs[0]["kind"], "inline");
        assert_eq!(eqs[0]["context"], "Setup");
        assert_eq!(eqs[1]["tex"], "\\int_0^1 f");
        assert_eq!(eqs[1]["kind"], "display");
    }

    #[tokio::test]
    #[ignore]
    async fn render_to_mathml_emits_math_element() {
        // Requires `latexml` to be installed on PATH. Run via
        // `cargo test --features ... -- --ignored render_to_mathml_emits_math_element`.
        let v = render_to_mathml("x+y").await;
        assert_eq!(v["ok"], json!(true), "got: {v:?}");
        let mml = v["mathml"].as_str().unwrap();
        assert!(mml.contains("<math"), "mathml={mml}");
        assert!(mml.contains("<mi>x</mi>"), "mathml={mml}");
    }

    #[tokio::test]
    async fn render_to_mathml_handles_latexml_absent() {
        // Point at a bogus binary so spawn() fails.
        std::env::set_var("GROKRXIV_LATEXML_BIN", "__no_such_binary_grokrxiv__");
        let v = render_to_mathml("x+y").await;
        std::env::remove_var("GROKRXIV_LATEXML_BIN");
        assert_eq!(v["ok"], json!(false), "got: {v:?}");
        let warnings = v["warnings"].as_array().unwrap();
        assert!(!warnings.is_empty(), "expected warning, got {v:?}");
    }

    #[test]
    fn equation_hash_stable_across_whitespace() {
        assert_eq!(equation_hash("a+b"), equation_hash("a +  b"));
        assert_eq!(equation_hash("a+b"), equation_hash(" a + b "));
    }

    #[test]
    fn equation_hash_differs_for_different_eqs() {
        assert_ne!(equation_hash("a+b"), equation_hash("a-b"));
        assert_ne!(equation_hash("x^2"), equation_hash("x^3"));
    }
}
