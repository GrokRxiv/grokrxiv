//! `arxiv_lookup({arxiv_id?})` — resolve an arXiv ID against the arXiv Atom API.
//!
//! GET `http://export.arxiv.org/api/query?id_list=<id>`. Returns
//! `{title, authors, abstract}` extracted from the Atom feed. If the `arxiv_id`
//! argument is omitted, the tool uses `ctx.source_id` so the LLM doesn't have
//! to repeat the obvious.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Implements `arxiv_lookup`.
pub struct ArxivLookupTool;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
/// Default arXiv Atom-feed endpoint.
pub const ARXIV_BASE: &str = "http://export.arxiv.org/api/query";

fn build_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "arxiv_id": {
                "type": "string",
                "description": "Optional. Defaults to the paper this agent is running against."
            }
        }
    })
}

#[async_trait]
impl Tool for ArxivLookupTool {
    fn name(&self) -> &'static str {
        "arxiv_lookup"
    }
    fn description(&self) -> &'static str {
        "Resolve metadata for an arXiv id via the arXiv Atom API."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let id = args
            .get("arxiv_id")
            .and_then(Value::as_str)
            .unwrap_or(ctx.source_id)
            .to_string();
        let base = std::env::var("GROKRXIV_ARXIV_BASE").unwrap_or_else(|_| ARXIV_BASE.to_string());
        let url = format!("{}?id_list={}", base, id);
        let resp = ctx
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("arxiv http: {e}"))?;
        if !resp.status().is_success() {
            return Ok(json!({
                "found": false,
                "http_status": resp.status().as_u16(),
            }));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("arxiv body: {e}"))?;
        // Pull the first <entry>...</entry>. Atom is regular enough that we
        // can scrape with substring matches; we avoid pulling in an XML
        // parser for this single-use tool.
        let entry = extract_first(&text, "<entry>", "</entry>").unwrap_or_default();
        let title = clean_field(&extract_first(&entry, "<title>", "</title>").unwrap_or_default());
        let summary =
            clean_field(&extract_first(&entry, "<summary>", "</summary>").unwrap_or_default());
        let authors: Vec<Value> = extract_all(&entry, "<author>", "</author>")
            .into_iter()
            .map(|a| {
                json!(clean_field(
                    &extract_first(&a, "<name>", "</name>").unwrap_or_default()
                ))
            })
            .collect();
        Ok(json!({
            "found": !title.is_empty(),
            "arxiv_id": id,
            "title": title,
            "authors": authors,
            "abstract": summary,
        }))
    }
}

fn extract_first(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let end_rel = haystack[start..].find(close)?;
    Some(haystack[start..start + end_rel].to_string())
}

fn extract_all(haystack: &str, open: &str, close: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = haystack;
    while let Some(s) = cur.find(open) {
        let after_open = &cur[s + open.len()..];
        if let Some(e) = after_open.find(close) {
            out.push(after_open[..e].to_string());
            cur = &after_open[e + close.len()..];
        } else {
            break;
        }
    }
    out
}

fn clean_field(s: &str) -> String {
    s.trim()
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
