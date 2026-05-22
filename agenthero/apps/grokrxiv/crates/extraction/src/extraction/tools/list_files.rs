//! `list_files(glob?)` — list files in the unpacked source bundle.
//!
//! Walks `ctx.workdir` recursively and returns `[{path, size}]`. The optional
//! `glob` is a simple matcher supporting `*` (any chars except `/`) and `**`
//! (any chars including `/`).

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Implements `list_files`.
pub struct ListFilesTool;

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &'static str {
        "list_files"
    }
    fn description(&self) -> &'static str {
        "List files in the unpacked source bundle (workdir). Optional `glob` filter."
    }
    fn schema(&self) -> &Value {
        // Return a reference to a static schema. We rebuild on each call —
        // serde_json::Value can't easily live in a `const`, and the schema is
        // tiny, so this is fine.
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let glob = args.get("glob").and_then(Value::as_str).map(str::to_owned);
        let mut entries: Vec<Value> = Vec::new();
        walk(ctx.workdir, ctx.workdir, &glob, &mut entries)?;
        entries.sort_by(|a, b| {
            let pa = a.get("path").and_then(Value::as_str).unwrap_or("");
            let pb = b.get("path").and_then(Value::as_str).unwrap_or("");
            pa.cmp(pb)
        });
        Ok(json!({ "files": entries }))
    }
}

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "glob": {
                "type": "string",
                "description": "Optional glob filter (e.g. `*.tex`, `figures/*`). Omit to list everything."
            }
        }
    })
}

fn walk(
    root: &std::path::Path,
    dir: &std::path::Path,
    glob: &Option<String>,
    out: &mut Vec<Value>,
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
            walk(root, &path, glob, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if let Some(g) = glob {
                if !glob_match(g, &rel) {
                    continue;
                }
            }
            out.push(json!({
                "path": rel,
                "size": meta.len(),
            }));
        }
    }
    Ok(())
}

/// Minimal glob matcher: `*` matches any chars except `/`; `**` matches any
/// chars including `/`; everything else literal.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_at(pattern.as_bytes(), 0, path.as_bytes(), 0)
}

fn glob_match_at(p: &[u8], mut pi: usize, s: &[u8], mut si: usize) -> bool {
    while pi < p.len() {
        let c = p[pi];
        if c == b'*' {
            if pi + 1 < p.len() && p[pi + 1] == b'*' {
                // `**` — any chars
                pi += 2;
                if pi == p.len() {
                    return true;
                }
                while si <= s.len() {
                    if glob_match_at(p, pi, s, si) {
                        return true;
                    }
                    si += 1;
                }
                return false;
            } else {
                pi += 1;
                if pi == p.len() {
                    // trailing single * must not cross /
                    return !s[si..].contains(&b'/');
                }
                while si <= s.len() {
                    if glob_match_at(p, pi, s, si) {
                        return true;
                    }
                    if si < s.len() && s[si] == b'/' {
                        return false;
                    }
                    si += 1;
                }
                return false;
            }
        } else if c == b'?' {
            if si >= s.len() || s[si] == b'/' {
                return false;
            }
            pi += 1;
            si += 1;
        } else {
            if si >= s.len() || s[si] != c {
                return false;
            }
            pi += 1;
            si += 1;
        }
    }
    si == s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_simple() {
        assert!(glob_match("*.tex", "main.tex"));
        assert!(!glob_match("*.tex", "src/main.tex"));
        assert!(glob_match("**/*.tex", "src/main.tex"));
        assert!(glob_match("figures/*", "figures/a.png"));
        assert!(!glob_match("figures/*", "figures/sub/a.png"));
        assert!(glob_match("figures/**", "figures/sub/a.png"));
    }
}
