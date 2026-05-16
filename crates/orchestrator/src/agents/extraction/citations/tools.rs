//! Citation-specific tools for [`super::CitationContextualizerAgent`].
//!
//! These are NOT in the shared core toolkit — they are scoped to Stage 7
//! and live under `extraction/citations/` to avoid coupling with the other
//! Wave-2 extraction agents.
//!
//! Four tools live here:
//! - `list_citation_sites` — scans `body.md` for every `[@key]` occurrence
//!   (including grouped `[@a; @b; @c]` form), tagging each site with its
//!   containing section and the surrounding sentence.
//! - `lookup_bibtex(key)` — finds the matching BibTeX entry under the workdir
//!   and parses out `{raw, doi?, arxiv_id?, title?, authors?, year?, venue?}`.
//! - `search_corpus(query, k?)` — best-effort semantic search over the
//!   GrokRxiv `papers` table. Degrades to ILIKE on title+abstract if pgvector
//!   isn't available; returns `[]` (no error) when the DB is unreachable.
//! - `read_section(id)` — citation-side copy of D4's tool; resolves a section
//!   id (`sec-N` 1-based, or a heading slug) against `body.md` and returns
//!   `{heading, body_markdown}`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{Tool, ToolCtx};

// =====================================================================
// list_citation_sites
// =====================================================================

/// Implements `list_citation_sites`.
pub struct ListCitationSitesTool;

static LIST_CITATION_SITES_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn list_citation_sites_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "description": "No arguments. Scans body.md for every [@key] (including grouped [@a; @b]) and returns one site per occurrence."
    })
}

#[async_trait]
impl Tool for ListCitationSitesTool {
    fn name(&self) -> &'static str {
        "list_citation_sites"
    }
    fn description(&self) -> &'static str {
        "Return every `[@key]` citation occurrence in body.md as {key, section, sentence, char_offset}."
    }
    fn schema(&self) -> &Value {
        LIST_CITATION_SITES_SCHEMA.get_or_init(list_citation_sites_schema)
    }
    async fn call(&self, _args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let body_path = ctx.workdir.join("body.md");
        let body = std::fs::read_to_string(&body_path)
            .map_err(|e| anyhow::anyhow!("list_citation_sites: cannot read body.md: {e}"))?;
        let sites = extract_citation_sites(&body);
        Ok(json!({ "sites": sites }))
    }
}

/// Pure scanner. Exposed `pub(super)` for unit tests.
pub(super) fn extract_citation_sites(body: &str) -> Vec<Value> {
    let sections = section_index(body);
    let bytes = body.as_bytes();
    let mut out: Vec<Value> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        // Find matching `]`.
        let Some(close_rel) = body[i + 1..].find(']') else {
            break;
        };
        let inner = &body[i + 1..i + 1 + close_rel];
        // Recognize citation groups: every token starts with `@`.
        let group_offset = i;
        let trimmed = inner.trim();
        if trimmed.starts_with('@') {
            // Split on ; while respecting optional whitespace.
            for raw_part in trimmed.split(';') {
                let p = raw_part.trim();
                let Some(rest) = p.strip_prefix('@') else { continue };
                let key = parse_cite_key(rest);
                if key.is_empty() {
                    continue;
                }
                let section = section_for_offset(group_offset, &sections);
                let sentence = surrounding_sentence(body, group_offset);
                out.push(json!({
                    "key": key,
                    "section": section,
                    "sentence": sentence,
                    "char_offset": group_offset as u64,
                }));
            }
        }
        i = i + 1 + close_rel + 1;
    }
    out
}

fn parse_cite_key(s: &str) -> String {
    // BibTeX-style keys: alphanumerics + `_` `-` `:` `.` `/`. Stop at the
    // first character outside that set so trailing punctuation in the
    // markdown doesn't leak into the key.
    let mut end = 0usize;
    for (idx, ch) in s.char_indices() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.' | '/') {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    s[..end].to_string()
}

/// Build a (heading, char_start) index by scanning `## ...` / `# ...` lines.
/// The section a site belongs to is the most-recent heading whose offset is
/// `<=` the site's offset.
fn section_index(body: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let trimmed_left = line.trim_start_matches(' ');
        if trimmed_left.starts_with('#') {
            let after_hashes: &str = trimmed_left.trim_start_matches('#');
            let heading = after_hashes.trim().trim_end_matches('\n').to_string();
            if !heading.is_empty() {
                out.push((heading, offset));
            }
        }
        offset += line.len();
    }
    out
}

fn section_for_offset(off: usize, sections: &[(String, usize)]) -> String {
    let mut best: Option<&str> = None;
    for (heading, start) in sections {
        if *start <= off {
            best = Some(heading.as_str());
        } else {
            break;
        }
    }
    best.unwrap_or("").to_string()
}

fn surrounding_sentence(body: &str, off: usize) -> String {
    let bytes = body.as_bytes();
    // Walk back to the previous sentence boundary.
    let mut start = 0usize;
    if off > 0 {
        let mut k = off.saturating_sub(1);
        loop {
            let b = bytes[k];
            if b == b'.' || b == b'?' || b == b'!' || b == b'\n' {
                start = k + 1;
                break;
            }
            if k == 0 {
                start = 0;
                break;
            }
            k -= 1;
        }
    }
    // Walk forward to the next sentence boundary.
    let mut end = bytes.len();
    let mut k = off;
    while k < bytes.len() {
        let b = bytes[k];
        if b == b'.' || b == b'?' || b == b'!' || b == b'\n' {
            end = (k + 1).min(bytes.len());
            break;
        }
        k += 1;
    }
    body[start..end].trim().to_string()
}

// =====================================================================
// lookup_bibtex
// =====================================================================

/// Implements `lookup_bibtex`.
pub struct LookupBibtexTool;

static LOOKUP_BIBTEX_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn lookup_bibtex_schema() -> Value {
    json!({
        "type": "object",
        "required": ["key"],
        "properties": {
            "key": { "type": "string", "description": "The BibTeX citation key (e.g. `smith2024`)." }
        }
    })
}

#[async_trait]
impl Tool for LookupBibtexTool {
    fn name(&self) -> &'static str {
        "lookup_bibtex"
    }
    fn description(&self) -> &'static str {
        "Find a BibTeX entry under the workdir by key. Returns {key, raw, doi?, arxiv_id?, title?, authors?, year?, venue?}."
    }
    fn schema(&self) -> &Value {
        LOOKUP_BIBTEX_SCHEMA.get_or_init(lookup_bibtex_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let key = args
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("lookup_bibtex requires `key`"))?;
        let bibs = find_bib_files(ctx.workdir)?;
        for path in bibs {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if let Some(entry) = find_bib_entry(&text, key) {
                return Ok(parse_bib_entry(key, &entry));
            }
        }
        anyhow::bail!("lookup_bibtex: key `{}` not found in any .bib file", key)
    }
}

fn find_bib_files(root: &std::path::Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("bib") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, &mut out).map_err(|e| anyhow::anyhow!("lookup_bibtex walk: {e}"))?;
    Ok(out)
}

/// Find `@<type>{<key>, ...}` block in a .bib file. Returns the raw block
/// (from `@` through the matching closing `}`).
pub(super) fn find_bib_entry(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Find the `{`.
        let Some(brace_rel) = text[i..].find('{') else {
            break;
        };
        let brace_abs = i + brace_rel;
        // Read the key (up to comma).
        let after_brace = &text[brace_abs + 1..];
        let comma_rel = after_brace.find(',').unwrap_or(after_brace.len().min(256));
        let candidate = after_brace[..comma_rel].trim();
        if candidate == key {
            // Find the matching `}`.
            let mut depth = 1i32;
            let mut k = brace_abs + 1;
            while k < bytes.len() {
                match bytes[k] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(text[i..=k].to_string());
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            return None;
        }
        i = brace_abs + 1;
    }
    None
}

/// Pull `title={...}` / `author={...}` / `doi={...}` / `eprint={...}` etc.
/// out of a BibTeX block. Best-effort — fields not present are absent in the
/// result (well, `null`-shaped for downstream JSON).
pub(super) fn parse_bib_entry(key: &str, raw: &str) -> Value {
    let title = bib_field(raw, "title");
    let authors_str = bib_field(raw, "author");
    let authors: Vec<Value> = match authors_str {
        Some(s) => s
            .split(" and ")
            .map(|a| Value::String(a.trim().to_string()))
            .collect(),
        None => Vec::new(),
    };
    let year = bib_field(raw, "year")
        .and_then(|y| y.trim().parse::<u64>().ok())
        .map(|n| Value::Number(n.into()))
        .unwrap_or(Value::Null);
    let venue = bib_field(raw, "journal")
        .or_else(|| bib_field(raw, "booktitle"))
        .or_else(|| bib_field(raw, "publisher"));
    let doi = bib_field(raw, "doi");
    let arxiv_id = bib_field(raw, "eprint")
        .or_else(|| bib_field(raw, "archiveprefix").and(bib_field(raw, "eprint")));
    json!({
        "key": key,
        "raw": raw,
        "doi": doi,
        "arxiv_id": arxiv_id,
        "title": title,
        "authors": authors,
        "year": year,
        "venue": venue,
    })
}

/// Find `<name> = {...}` or `<name> = "..."` in a BibTeX block.
fn bib_field(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    let needle = name.to_lowercase();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let start = search_from + rel;
        // Check the character before is a separator (start of block, comma, whitespace).
        let pre_ok = start == 0
            || matches!(
                lower.as_bytes()[start - 1],
                b',' | b'{' | b' ' | b'\n' | b'\t'
            );
        let after = &raw[start + needle.len()..];
        // Look for `=` after optional whitespace.
        let trimmed = after.trim_start();
        if pre_ok && trimmed.starts_with('=') {
            let after_eq = trimmed[1..].trim_start();
            return Some(read_bib_value(after_eq));
        }
        search_from = start + needle.len();
    }
    None
}

fn read_bib_value(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return String::new();
    }
    if bytes[0] == b'{' {
        let mut depth = 1i32;
        let mut k = 1usize;
        while k < bytes.len() {
            match bytes[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return s[1..k].trim().to_string();
                    }
                }
                _ => {}
            }
            k += 1;
        }
        return s[1..].trim().to_string();
    }
    if bytes[0] == b'"' {
        if let Some(end) = s[1..].find('"') {
            return s[1..1 + end].trim().to_string();
        }
    }
    // Bare value: stop at comma / newline / closing brace.
    let end = s.find([',', '\n', '}']).unwrap_or(s.len());
    s[..end].trim().to_string()
}

// =====================================================================
// search_corpus
// =====================================================================

/// Implements `search_corpus`. Degrades gracefully when the DB is
/// unreachable: returns `[]` with no error so the agent can still proceed.
pub struct SearchCorpusTool;

static SEARCH_CORPUS_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn search_corpus_schema() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "description": "Free-text query." },
            "k": { "type": "integer", "description": "Max results (default 5)." }
        }
    })
}

#[async_trait]
impl Tool for SearchCorpusTool {
    fn name(&self) -> &'static str {
        "search_corpus"
    }
    fn description(&self) -> &'static str {
        "Search already-ingested GrokRxiv papers by title/abstract. Returns up to k matches with score."
    }
    fn schema(&self) -> &Value {
        SEARCH_CORPUS_SCHEMA.get_or_init(search_corpus_schema)
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("search_corpus requires `query`"))?;
        let k = args
            .get("k")
            .and_then(Value::as_i64)
            .map(|n| n.clamp(1, 50) as usize)
            .unwrap_or(5);

        let Ok(db_url) = std::env::var("DATABASE_URL") else {
            return Ok(json!({ "results": [] }));
        };

        let results = run_corpus_search(&db_url, query, k).await.unwrap_or_default();
        Ok(json!({ "results": results }))
    }
}

async fn run_corpus_search(
    db_url: &str,
    query: &str,
    k: usize,
) -> anyhow::Result<Vec<Value>> {
    use sqlx::postgres::PgPoolOptions;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(db_url)
        .await?;
    let pattern = format!("%{}%", query);
    // ILIKE fallback — pgvector path can be wired later when an `embedding`
    // column lands on `papers`. Today's schema (papers: id/arxiv_id/title/
    // authors/abstract/field/...) only supports lexical search.
    let rows = sqlx::query_as::<_, (
        uuid::Uuid,
        String,
        String,
        Option<String>,
    )>(
        "SELECT id, arxiv_id, title, abstract \
         FROM papers \
         WHERE title ILIKE $1 OR abstract ILIKE $1 \
         LIMIT $2",
    )
    .bind(&pattern)
    .bind(k as i64)
    .fetch_all(&pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, arxiv_id, title, abs_)| {
            let snippet = abs_
                .as_deref()
                .map(|s| s.chars().take(240).collect::<String>())
                .unwrap_or_default();
            json!({
                "paper_id": id.to_string(),
                "arxiv_id": arxiv_id,
                "title": title,
                "snippet": snippet,
                "score": 1.0_f64,
            })
        })
        .collect())
}

// =====================================================================
// read_section
// =====================================================================

/// Implements `read_section`. Citation-side copy — D4's `theorem_graph`
/// extractor has its own; coordinated decoupling per the team brief.
pub struct ReadSectionTool;

static READ_SECTION_SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn read_section_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Section id. `sec-N` (1-based, in order of appearance) or a heading slug."
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
        "Read one section of body.md. id is `sec-N` (1-based) or a heading slug. Returns {heading, body_markdown}."
    }
    fn schema(&self) -> &Value {
        READ_SECTION_SCHEMA.get_or_init(read_section_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("read_section requires `id`"))?;
        let body_path = ctx.workdir.join("body.md");
        let body = std::fs::read_to_string(&body_path)
            .map_err(|e| anyhow::anyhow!("read_section: cannot read body.md: {e}"))?;
        let section = resolve_section(&body, id)
            .ok_or_else(|| anyhow::anyhow!("read_section: section `{}` not found", id))?;
        Ok(json!({
            "heading": section.0,
            "body_markdown": section.1,
        }))
    }
}

/// Returns `(heading, body)` for the section identified by `id`.
pub(super) fn resolve_section(body: &str, id: &str) -> Option<(String, String)> {
    let sections: Vec<(String, usize)> = section_index(body);
    if sections.is_empty() {
        return None;
    }
    // Determine which section index the caller wants.
    let want_idx: Option<usize> = if let Some(rest) = id.strip_prefix("sec-") {
        rest.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
    } else {
        sections.iter().position(|(h, _)| slugify(h) == slugify(id) || h == id)
    };
    let idx = want_idx?;
    if idx >= sections.len() {
        return None;
    }
    let (heading, start) = sections[idx].clone();
    let end = sections
        .get(idx + 1)
        .map(|(_, e)| *e)
        .unwrap_or(body.len());
    // Skip the heading line itself.
    let heading_line_end = body[start..]
        .find('\n')
        .map(|n| start + n + 1)
        .unwrap_or(end);
    let body_md = body[heading_line_end.min(end)..end].trim_matches('\n').to_string();
    Some((heading, body_md))
}

fn slugify(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn tmpdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("grokrxiv-d5-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ctx_for(workdir: &std::path::Path) -> ToolCtx<'_> {
        ToolCtx {
            workdir,
            semantic_ast: None,
            arxiv_id: "2401.99999",
            http: Arc::new(reqwest::Client::new()),
        }
    }

    #[test]
    fn list_citation_sites_extracts_single() {
        let body = "## Intro\nIn [@foo2024], we see X.\n";
        let sites = extract_citation_sites(body);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0]["key"], "foo2024");
    }

    #[test]
    fn list_citation_sites_extracts_grouped() {
        let body = "Background work [@foo; @bar; @baz] is large.\n";
        let sites = extract_citation_sites(body);
        assert_eq!(sites.len(), 3);
        let keys: Vec<&str> = sites
            .iter()
            .map(|s| s["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn list_citation_sites_assigns_section() {
        let body = "# Title\nstuff.\n\n## Intro\n\nWe build on [@foo].\n\n## Methods\n\nMore [@bar].\n";
        let sites = extract_citation_sites(body);
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["section"], "Intro");
        assert_eq!(sites[1]["section"], "Methods");
    }

    #[tokio::test]
    async fn lookup_bibtex_finds_entry() {
        let dir = tmpdir();
        let bib = "@article{foo, title={X-Ray Scaling}, author={Alice and Bob}, year={2024}, doi={10.1/abc}, journal={Nat. Phys.}}";
        std::fs::write(dir.join("refs.bib"), bib).unwrap();
        let tool = LookupBibtexTool;
        let result = tool
            .call(json!({"key": "foo"}), &ctx_for(&dir))
            .await
            .unwrap();
        assert_eq!(result["key"], "foo");
        assert_eq!(result["doi"], "10.1/abc");
        assert_eq!(result["title"], "X-Ray Scaling");
        assert_eq!(result["year"], 2024);
        assert_eq!(result["venue"], "Nat. Phys.");
        let authors = result["authors"].as_array().unwrap();
        assert_eq!(authors.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn lookup_bibtex_missing_returns_error() {
        let dir = tmpdir();
        std::fs::write(dir.join("refs.bib"), "@article{other, title={Y}}").unwrap();
        let tool = LookupBibtexTool;
        let err = tool
            .call(json!({"key": "unknown"}), &ctx_for(&dir))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn search_corpus_returns_empty_when_db_unavailable() {
        // Stash any real DATABASE_URL so the test runs deterministically.
        let prior = std::env::var("DATABASE_URL").ok();
        std::env::remove_var("DATABASE_URL");
        let dir = tmpdir();
        let tool = SearchCorpusTool;
        let result = tool
            .call(json!({"query": "anything"}), &ctx_for(&dir))
            .await
            .unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 0);
        if let Some(v) = prior {
            std::env::set_var("DATABASE_URL", v);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_section_returns_body() {
        let dir = tmpdir();
        let body = "# Title\n\nlead.\n\n## Intro\n\nfirst-body.\n\n## Methods\n\nsecond-body.\n";
        std::fs::write(dir.join("body.md"), body).unwrap();
        let tool = ReadSectionTool;
        let result = tool
            .call(json!({"id": "sec-2"}), &ctx_for(&dir))
            .await
            .unwrap();
        assert_eq!(result["heading"], "Intro");
        let bm = result["body_markdown"].as_str().unwrap();
        assert!(bm.contains("first-body"), "got {bm:?}");
        assert!(!bm.contains("second-body"), "leaked next section");
        std::fs::remove_dir_all(&dir).ok();
    }
}
