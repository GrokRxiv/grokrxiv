//! `crossref_lookup({title?, doi?, arxiv_id?})` — resolve a citation against
//! CrossRef.
//!
//! GET `https://api.crossref.org/works?...`. Returns the top match's
//! `{title, authors, venue, year, doi}`. If CrossRef returns no items, the
//! tool surfaces `{found: false}` to the LLM.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Implements `crossref_lookup`.
pub struct CrossrefLookupTool;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
/// Public so tests can override the base URL via a wiremock mount.
pub const CROSSREF_BASE: &str = "https://api.crossref.org/works";

fn build_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "doi":   { "type": "string" },
            "arxiv_id": { "type": "string" }
        }
    })
}

#[async_trait]
impl Tool for CrossrefLookupTool {
    fn name(&self) -> &'static str {
        "crossref_lookup"
    }
    fn description(&self) -> &'static str {
        "Resolve a citation against the CrossRef Works API. Returns the top match."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let title = args.get("title").and_then(Value::as_str);
        let doi = args.get("doi").and_then(Value::as_str);
        let arxiv = args.get("arxiv_id").and_then(Value::as_str);

        // Operator-overridable base URL (used by wiremock tests).
        let base =
            std::env::var("GROKRXIV_CROSSREF_BASE").unwrap_or_else(|_| CROSSREF_BASE.to_string());
        let url = if let Some(d) = doi {
            format!("{}/{}", base, urlencode(d))
        } else if let Some(a) = arxiv {
            format!("{}?query.bibliographic=arXiv:{}&rows=1", base, urlencode(a))
        } else if let Some(t) = title {
            format!("{}?query.title={}&rows=1", base, urlencode(t))
        } else {
            anyhow::bail!("crossref_lookup requires one of title/doi/arxiv_id");
        };

        let resp = ctx
            .http
            .get(&url)
            .header("user-agent", "grokrxiv-extraction/0.1")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("crossref http: {e}"))?;
        if !resp.status().is_success() {
            return Ok(json!({
                "found": false,
                "http_status": resp.status().as_u16(),
            }));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("crossref json: {e}"))?;
        // CrossRef envelope: `{"message": {...}}`. For DOI it's the work directly;
        // for query it's `{"items": [...]}`.
        let msg = body.get("message").cloned().unwrap_or(Value::Null);
        let work = if let Some(items) = msg.get("items").and_then(Value::as_array) {
            items.first().cloned().unwrap_or(Value::Null)
        } else {
            msg
        };
        if work.is_null() {
            return Ok(json!({ "found": false }));
        }
        let title = work
            .get("title")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
        let authors: Vec<Value> = work
            .get("author")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let given = a.get("given").and_then(Value::as_str).unwrap_or("");
                let family = a.get("family").and_then(Value::as_str).unwrap_or("");
                json!(format!("{given} {family}").trim().to_string())
            })
            .collect();
        let venue = work
            .get("container-title")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
        let year = work
            .get("issued")
            .and_then(|i| i.get("date-parts"))
            .and_then(|d| d.get(0))
            .and_then(|p| p.get(0))
            .and_then(Value::as_u64);
        let doi = work.get("DOI").and_then(Value::as_str).map(str::to_owned);
        Ok(json!({
            "found": true,
            "title": title,
            "authors": authors,
            "venue": venue,
            "year": year,
            "doi": doi,
        }))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
