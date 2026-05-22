//! `openalex_lookup({doi?, title?})` — resolve citation metadata via OpenAlex.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Implements `openalex_lookup`.
pub struct OpenAlexLookupTool;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
/// Default OpenAlex Works API endpoint.
pub const OPENALEX_BASE: &str = "https://api.openalex.org/works";

fn build_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "doi": { "type": "string" },
            "title": { "type": "string" }
        }
    })
}

#[async_trait]
impl Tool for OpenAlexLookupTool {
    fn name(&self) -> &'static str {
        "openalex_lookup"
    }
    fn description(&self) -> &'static str {
        "Resolve citation metadata via OpenAlex Works. Returns the top match."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let doi = args.get("doi").and_then(Value::as_str);
        let title = args.get("title").and_then(Value::as_str);
        let base =
            std::env::var("GROKRXIV_OPENALEX_BASE").unwrap_or_else(|_| OPENALEX_BASE.to_string());
        let url = if let Some(doi) = doi {
            format!("{}/doi:{}", base, urlencode(doi))
        } else if let Some(title) = title {
            format!("{}?search={}&per-page=1", base, urlencode(title))
        } else {
            anyhow::bail!("openalex_lookup requires doi or title");
        };

        let resp = ctx
            .http
            .get(&url)
            .header("user-agent", "grokrxiv-extraction/0.1")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("openalex http: {e}"))?;
        if !resp.status().is_success() {
            return Ok(json!({
                "found": false,
                "http_status": resp.status().as_u16(),
            }));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("openalex json: {e}"))?;
        let work = body
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| results.first())
            .cloned()
            .unwrap_or(body);
        if work.is_null() {
            return Ok(json!({ "found": false }));
        }
        let authors: Vec<Value> = work
            .get("authorships")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|authorship| {
                authorship
                    .get("author")
                    .and_then(|author| author.get("display_name"))
                    .and_then(Value::as_str)
                    .map(|name| json!(name))
            })
            .collect();
        Ok(json!({
            "found": true,
            "title": work.get("title").and_then(Value::as_str),
            "authors": authors,
            "venue": work
                .get("primary_location")
                .and_then(|location| location.get("source"))
                .and_then(|source| source.get("display_name"))
                .and_then(Value::as_str),
            "year": work.get("publication_year").and_then(Value::as_u64),
            "doi": work.get("doi").and_then(Value::as_str),
            "openalex_id": work.get("id").and_then(Value::as_str),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_doi_or_title_arguments() {
        let schema = build_schema();
        assert!(schema["properties"].get("doi").is_some());
        assert!(schema["properties"].get("title").is_some());
    }
}
