//! `query_ast(jsonpath)` — run a JSONPath query against the semantic AST.
//!
//! The semantic AST is populated upstream by the deterministic Stage 2
//! (Pandoc plus LaTeXML). When it's unavailable the tool reports that to the
//! LLM rather than failing the whole loop.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{Tool, ToolCtx};

/// Implements `query_ast`.
pub struct QueryAstTool;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_schema() -> Value {
    json!({
        "type": "object",
        "required": ["jsonpath"],
        "properties": {
            "jsonpath": {
                "type": "string",
                "description": "Subset JSONPath. Supports `$`, `.`, `[k]`, `[*]`, `..key` (recursive descent)."
            }
        }
    })
}

#[async_trait]
impl Tool for QueryAstTool {
    fn name(&self) -> &'static str {
        "query_ast"
    }
    fn description(&self) -> &'static str {
        "Run a JSONPath query against the paper's semantic_ast JSON."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let q = args
            .get("jsonpath")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("query_ast requires `jsonpath`"))?;
        let Some(ast) = ctx.semantic_ast else {
            return Ok(json!({
                "matches": [],
                "warning": "semantic_ast not available — deterministic Stage 2 did not run on this paper"
            }));
        };
        let matches = jsonpath_query(ast, q)?;
        Ok(json!({ "matches": matches }))
    }
}

/// Minimal JSONPath subset: `$`, `.key`, `[index]`, `[*]`, `..key`.
pub(crate) fn jsonpath_query(root: &Value, path: &str) -> anyhow::Result<Vec<Value>> {
    let mut tokens = tokenize(path)?;
    let mut cursor: Vec<Value> = vec![root.clone()];
    while let Some(tok) = tokens.pop_front() {
        let mut next: Vec<Value> = Vec::new();
        match tok {
            Token::Root => {
                // Root is a no-op: cursor stays as-is.
                continue;
            }
            Token::Field(k) => {
                for v in &cursor {
                    if let Some(child) = v.get(&k) {
                        next.push(child.clone());
                    }
                }
            }
            Token::Index(i) => {
                for v in &cursor {
                    if let Some(child) = v.get(i) {
                        next.push(child.clone());
                    }
                }
            }
            Token::WildIndex => {
                for v in &cursor {
                    if let Some(arr) = v.as_array() {
                        next.extend(arr.iter().cloned());
                    }
                }
            }
            Token::Descend(k) => {
                for v in &cursor {
                    collect_descendants(v, &k, &mut next);
                }
            }
        }
        cursor = next;
    }
    Ok(cursor)
}

fn collect_descendants(v: &Value, key: &str, out: &mut Vec<Value>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k == key {
                    out.push(child.clone());
                }
                collect_descendants(child, key, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                collect_descendants(child, key, out);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
enum Token {
    Root,
    Field(String),
    Index(usize),
    WildIndex,
    Descend(String),
}

fn tokenize(path: &str) -> anyhow::Result<std::collections::VecDeque<Token>> {
    let mut out = std::collections::VecDeque::new();
    let bytes = path.as_bytes();
    let mut i = 0;
    if i < bytes.len() && bytes[i] == b'$' {
        out.push_back(Token::Root);
        i += 1;
    }
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                    i += 2;
                    let start = i;
                    while i < bytes.len() && is_ident_byte(bytes[i]) {
                        i += 1;
                    }
                    if start == i {
                        anyhow::bail!("expected identifier after `..` in path");
                    }
                    out.push_back(Token::Descend(
                        String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                    ));
                } else {
                    i += 1;
                    let start = i;
                    while i < bytes.len() && is_ident_byte(bytes[i]) {
                        i += 1;
                    }
                    if start == i {
                        anyhow::bail!("expected identifier after `.` in path");
                    }
                    out.push_back(Token::Field(
                        String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                    ));
                }
            }
            b'[' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'*' {
                    out.push_back(Token::WildIndex);
                    i += 1;
                } else if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                    let quote = bytes[i];
                    i += 1;
                    let start = i;
                    while i < bytes.len() && bytes[i] != quote {
                        i += 1;
                    }
                    out.push_back(Token::Field(
                        String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                    ));
                    i += 1;
                } else {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if start == i {
                        anyhow::bail!("expected index, '*' or quoted key after `[`");
                    }
                    let n: usize = std::str::from_utf8(&bytes[start..i])
                        .unwrap()
                        .parse()
                        .map_err(|e| anyhow::anyhow!("bad index: {e}"))?;
                    out.push_back(Token::Index(n));
                }
                if i >= bytes.len() || bytes[i] != b']' {
                    anyhow::bail!("expected `]`");
                }
                i += 1;
            }
            _ => anyhow::bail!("unexpected character in jsonpath at byte {i}"),
        }
    }
    Ok(out)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jsonpath_basic() {
        let v = json!({
            "body": {
                "theorem": [
                    {"id": "t1", "text": "T1"},
                    {"id": "t2", "text": "T2"}
                ]
            }
        });
        let r = jsonpath_query(&v, "$.body.theorem[*]").unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["id"], "t1");
        let r = jsonpath_query(&v, "$..id").unwrap();
        assert_eq!(r.len(), 2);
    }
}
