//! Citation existence verifier.
//!
//! Every citation gets a real existence check:
//!   - `Citation.doi`        → `GET {crossref_base}/{doi}` (metadata only).
//!   - `Citation.arxiv_id`   → batched `GET {arxiv_base}?id_list=ID1,ID2,...`,
//!                             a single Atom response covers up to 100 ids.
//!   - Plain refs (no DOI, no arxiv_id) → crossref free-text bibliographic
//!                             query against `Citation.raw`; the top hit
//!                             counts as resolved when its score crosses the
//!                             threshold below.
//!
//! Results are memoised in-process by lookup URL so a paper with repeated
//! citations only spends one network round-trip per unique key. The verifier
//! deliberately stays metadata-only — never requests PDFs or LaTeX.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{Verifier, VerifierContext};

/// Mark Fail if more than this fraction of citations are unresolved.
const FAIL_FRACTION: f32 = 0.30;

/// Minimum crossref `score` for a free-text bibliographic match to count as
/// resolved. Crossref's score ranges roughly 0-200 for a top hit; ~60 is the
/// rule-of-thumb floor where the result is meaningfully relevant.
const BIBLIOGRAPHIC_MATCH_SCORE_MIN: f64 = 60.0;

/// Citation verifier. Caches results across verify() calls within the process.
pub struct CitationVerifier {
    /// Base URL for Crossref `/works/{doi}` lookups.
    crossref_base: String,
    /// Base URL for arXiv id-list metadata queries (Atom feed).
    arxiv_base: String,
    cache: Arc<Mutex<HashMap<String, bool>>>,
    /// Resolved bibliographic queries: key = `raw` string, value = the DOI
    /// crossref returned if the score was high enough.
    biblio_cache: Arc<Mutex<HashMap<String, Option<String>>>>,
}

impl Default for CitationVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl CitationVerifier {
    /// Construct a verifier pointed at the real Crossref + arXiv endpoints.
    pub fn new() -> Self {
        Self {
            crossref_base: "https://api.crossref.org/works".to_string(),
            arxiv_base: "https://export.arxiv.org/api/query".to_string(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with custom base URLs; intended for tests against
    /// `wiremock`.
    pub fn with_bases(crossref_base: impl Into<String>) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            arxiv_base: "https://export.arxiv.org/api/query".to_string(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with both bases overridden — used by tests that
    /// also need to mock the arXiv endpoint.
    pub fn with_all_bases(
        crossref_base: impl Into<String>,
        arxiv_base: impl Into<String>,
    ) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            arxiv_base: arxiv_base.into(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn resolve_doi(&self, http: &reqwest::Client, doi: &str) -> bool {
        let url = format!("{}/{doi}", self.crossref_base);
        if let Some(v) = self.cache.lock().get(&url) {
            return *v;
        }
        let ok = match http.get(&url).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        };
        self.cache.lock().insert(url, ok);
        ok
    }

    /// Batched arXiv existence check. Calls `{arxiv_base}?id_list=id1,id2,...`
    /// once and returns the set of ids the server responded with. arXiv
    /// returns an Atom feed; we parse it permissively by scanning for the
    /// `<id>http(s)://arxiv.org/abs/{id}</id>` lines.
    async fn resolve_arxiv_ids(
        &self,
        http: &reqwest::Client,
        ids: &[String],
    ) -> HashSet<String> {
        if ids.is_empty() {
            return HashSet::new();
        }
        // De-dup + filter for in-cache + shape-validate before hitting the wire.
        let mut to_query: Vec<String> = Vec::new();
        let mut resolved: HashSet<String> = HashSet::new();
        for id in ids {
            let cache_key = format!("arxiv::{id}");
            if let Some(true) = self.cache.lock().get(&cache_key).copied() {
                resolved.insert(id.clone());
                continue;
            }
            if let Some(false) = self.cache.lock().get(&cache_key).copied() {
                continue;
            }
            if !Self::arxiv_id_well_formed(id) {
                self.cache.lock().insert(cache_key, false);
                continue;
            }
            to_query.push(strip_version(id).to_string());
        }
        if to_query.is_empty() {
            return resolved;
        }
        let url = format!("{}?id_list={}", self.arxiv_base, to_query.join(","));
        let body = match http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
            _ => String::new(),
        };
        // The Atom response contains one `<entry>` per id that resolved. We
        // scan for `<id>http(s)://arxiv.org/abs/{id}` substrings — robust to
        // namespace prefix differences and avoids pulling in an XML parser.
        for q in &to_query {
            let needle_https = format!("arxiv.org/abs/{q}");
            let cache_key = format!("arxiv::{q}");
            if body.contains(&needle_https) {
                self.cache.lock().insert(cache_key, true);
                resolved.insert(q.clone());
            } else {
                self.cache.lock().insert(cache_key, false);
            }
        }
        // Also accept the versioned variants (`{id}v2`) when the caller asked
        // for one — arXiv resolves them to the same underlying entry.
        for original in ids {
            if resolved.contains(strip_version(original)) {
                resolved.insert(original.clone());
            }
        }
        resolved
    }

    /// Free-text crossref bibliographic lookup for refs that carry neither a
    /// DOI nor an arxiv_id. Returns the resolved DOI when crossref's top hit
    /// scores above `BIBLIOGRAPHIC_MATCH_SCORE_MIN`. Cached by `raw` string.
    async fn resolve_bibliographic(
        &self,
        http: &reqwest::Client,
        raw: &str,
    ) -> Option<String> {
        if raw.trim().is_empty() {
            return None;
        }
        if let Some(v) = self.biblio_cache.lock().get(raw) {
            return v.clone();
        }
        let url = format!("{}?rows=1&query.bibliographic=", self.crossref_base);
        let encoded = url_form_encode(raw);
        let full = format!("{url}{encoded}");
        let resolved = match http.get(&full).send().await {
            Ok(r) if r.status().is_success() => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| top_doi_if_scored(&v, BIBLIOGRAPHIC_MATCH_SCORE_MIN)),
            _ => None,
        };
        self.biblio_cache.lock().insert(raw.to_string(), resolved.clone());
        resolved
    }

    /// Check that an arXiv id has a syntactically valid shape WITHOUT making a
    /// network request. Real existence checks must go through the
    /// `grokrxiv-ingest` rate-gate; this verifier deliberately stays
    /// transport-free for arXiv references.
    fn arxiv_id_well_formed(id: &str) -> bool {
        // Strip an optional `vN` version suffix.
        let (core, version_ok) = match id.rfind('v') {
            Some(idx) if idx > 0 => {
                let tail = &id[idx + 1..];
                if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                    (&id[..idx], true)
                } else {
                    (id, true)
                }
            }
            _ => (id, true),
        };
        if !version_ok {
            return false;
        }
        // Legacy form contains `/` — disambiguate first so `math.AG/0301001`
        // doesn't get parsed as `YYMM.NNNNN`.
        if core.contains('/') {
            let Some((subject, num)) = core.split_once('/') else {
                return false;
            };
            return num.len() == 7
                && num.chars().all(|c| c.is_ascii_digit())
                && !subject.is_empty();
        }
        // Modern: `YYMM.NNNNN`.
        let Some((yymm, num)) = core.split_once('.') else {
            return false;
        };
        yymm.len() == 4
            && yymm.chars().all(|c| c.is_ascii_digit())
            && (num.len() == 4 || num.len() == 5)
            && num.chars().all(|c| c.is_ascii_digit())
    }
}

#[async_trait]
impl Verifier for CitationVerifier {
    fn name(&self) -> &'static str {
        "citation"
    }

    async fn verify(
        &self,
        _artifact: &serde_json::Value,
        ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        // Phase 1: batch the arXiv-id-only refs so we hit the arXiv API once.
        let arxiv_ids: Vec<String> = ctx
            .paper
            .bibliography
            .iter()
            .filter_map(|c| {
                if c.doi.is_none() {
                    c.arxiv_id.clone()
                } else {
                    None
                }
            })
            .collect();
        let arxiv_resolved = self.resolve_arxiv_ids(ctx.http, &arxiv_ids).await;

        // Phase 2: walk every citation, recording per-entry resolution so the
        // supervisor can overlay this onto the LLM specialist's citation_review
        // output before persisting.
        let mut total: u32 = 0;
        let mut unresolved: Vec<String> = Vec::new();
        let mut resolved_via_biblio: u32 = 0;
        let mut entries: Vec<serde_json::Value> = Vec::with_capacity(ctx.paper.bibliography.len());
        for c in &ctx.paper.bibliography {
            total += 1;
            let mut resolved_doi: Option<String> = None;
            let mut resolved_url: Option<String> = None;
            let mut source: Option<&'static str> = None;

            if let Some(doi) = &c.doi {
                if self.resolve_doi(ctx.http, doi).await {
                    resolved_doi = Some(doi.clone());
                    resolved_url = Some(format!("https://doi.org/{doi}"));
                    source = Some("crossref");
                }
            }
            if source.is_none() {
                if let Some(arxiv_id) = &c.arxiv_id {
                    if arxiv_resolved.contains(arxiv_id) {
                        let canonical = strip_version(arxiv_id);
                        resolved_url = Some(format!("https://arxiv.org/abs/{canonical}"));
                        source = Some("arxiv");
                    }
                }
            }
            if source.is_none() && c.doi.is_none() && c.arxiv_id.is_none() {
                if let Some(doi) = self.resolve_bibliographic(ctx.http, &c.raw).await {
                    resolved_doi = Some(doi.clone());
                    resolved_url = Some(format!("https://doi.org/{doi}"));
                    source = Some("crossref_bibliographic");
                    resolved_via_biblio += 1;
                }
            }
            let exists = source.is_some();
            if !exists {
                unresolved.push(c.raw.clone());
            }
            entries.push(json!({
                "raw": c.raw,
                "exists": exists,
                "resolved_doi": resolved_doi,
                "resolved_url": resolved_url,
                "source": source.unwrap_or("none"),
            }));
        }

        if total == 0 {
            return VerifierResult {
                status: VerifierStatus::Pass,
                notes: json!({ "checked": 0, "entries": [] }),
            };
        }
        let frac = unresolved.len() as f32 / total as f32;
        let status = if unresolved.is_empty() {
            VerifierStatus::Pass
        } else if frac > FAIL_FRACTION {
            VerifierStatus::Fail
        } else {
            VerifierStatus::Warn
        };
        VerifierResult {
            status,
            notes: json!({
                "checked": total,
                "unresolved": unresolved,
                "unresolved_fraction": frac,
                "resolved_via_bibliographic_query": resolved_via_biblio,
                "entries": entries,
            }),
        }
    }
}

/// Strip a trailing `vN` arXiv version suffix. `2401.12345v2` → `2401.12345`.
fn strip_version(id: &str) -> &str {
    let Some(idx) = id.rfind('v') else { return id };
    if idx == 0 {
        return id;
    }
    let tail = &id[idx + 1..];
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return id;
    }
    &id[..idx]
}

/// Application/x-www-form-urlencoded encoder limited to the characters we
/// need for a crossref bibliographic query. Pulled inline so the verifier
/// crate doesn't take an extra dep just for URL encoding.
fn url_form_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Parse a crossref `/works` response and return the top hit's DOI iff its
/// `score` is at or above `min_score`.
fn top_doi_if_scored(body: &serde_json::Value, min_score: f64) -> Option<String> {
    let item = body
        .get("message")?
        .get("items")?
        .as_array()?
        .first()?;
    let score = item.get("score")?.as_f64()?;
    if score < min_score {
        return None;
    }
    item.get("DOI")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::{Citation, PaperExtract};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn paper_with(cites: Vec<Citation>) -> PaperExtract {
        PaperExtract {
            arxiv_id: "x".into(),
            title: "t".into(),
            authors: vec![],
            abstract_: "a".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: cites,
            source_format: None,
        }
    }

    #[tokio::test]
    async fn no_citations_passes() {
        let v = CitationVerifier::new();
        let paper = paper_with(vec![]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass));
    }

    #[tokio::test]
    async fn warns_when_a_citation_is_unresolved() {
        let server = MockServer::start().await;
        // Resolved DOI.
        Mock::given(method("GET"))
            .and(path("/works/10.good/doi"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // Unresolved DOI.
        Mock::given(method("GET"))
            .and(path("/works/10.bad/doi"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bases(format!("{}/works", server.uri()));
        let paper = paper_with(vec![
            Citation {
                raw: "Good".into(),
                doi: Some("10.good/doi".into()),
                arxiv_id: None,
                title: None,
            },
            Citation {
                raw: "Bad".into(),
                doi: Some("10.bad/doi".into()),
                arxiv_id: None,
                title: None,
            },
        ]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        // 50% unresolved > 30% → Fail.
        assert!(matches!(r.status, VerifierStatus::Fail));
        assert_eq!(r.notes["checked"], 2);
    }

    #[test]
    fn arxiv_id_shape_checker() {
        assert!(CitationVerifier::arxiv_id_well_formed("2605.12484"));
        assert!(CitationVerifier::arxiv_id_well_formed("2401.12345v2"));
        assert!(CitationVerifier::arxiv_id_well_formed("math.AG/0301001"));
        assert!(!CitationVerifier::arxiv_id_well_formed("not-an-arxiv-id"));
        assert!(!CitationVerifier::arxiv_id_well_formed("2605.12"));
    }

    #[tokio::test]
    async fn arxiv_batch_query_marks_present_ids_resolved() {
        // arXiv server returns an Atom feed containing entries for the two
        // ids we query — the verifier should accept both, and the entry that
        // was NOT in the response should land in `unresolved`.
        let server = MockServer::start().await;
        let atom = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v2</id>
    <title>Real paper</title>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/math.AG/0301001</id>
    <title>Real legacy paper</title>
  </entry>
</feed>"#;
        Mock::given(method("GET"))
            .and(path("/api/query"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(atom)
                    .insert_header("content-type", "application/atom+xml"),
            )
            .mount(&server)
            .await;
        let v = CitationVerifier::with_all_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
        );
        let paper = paper_with(vec![
            Citation {
                raw: "Real".into(),
                doi: None,
                arxiv_id: Some("2401.12345v2".into()),
                title: None,
            },
            Citation {
                raw: "Legacy".into(),
                doi: None,
                arxiv_id: Some("math.AG/0301001".into()),
                title: None,
            },
            Citation {
                raw: "Fake".into(),
                doi: None,
                arxiv_id: Some("9999.99999".into()),
                title: None,
            },
        ]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        // 1 / 3 = 33% unresolved → Fail (threshold is 30%).
        assert!(matches!(r.status, VerifierStatus::Fail), "got {:?}", r.status);
        assert_eq!(r.notes["checked"], 3);
        let unresolved = r.notes["unresolved"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0], "Fake");
    }

    #[tokio::test]
    async fn bibliographic_query_resolves_plain_refs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.found/plain-ref",
                        "score": 95.0,
                        "title": ["Found Title"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        let v = CitationVerifier::with_bases(format!("{}/works", server.uri()));
        let paper = paper_with(vec![Citation {
            raw: "Some bibliographic entry text".into(),
            doi: None,
            arxiv_id: None,
            title: None,
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass), "got {:?}", r.status);
        assert_eq!(r.notes["checked"], 1);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 1);
    }

    #[tokio::test]
    async fn bibliographic_query_below_threshold_stays_unresolved() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weakmatch/foo",
                        "score": 12.0,
                        "title": ["Weak Match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        let v = CitationVerifier::with_bases(format!("{}/works", server.uri()));
        let paper = paper_with(vec![Citation {
            raw: "Plain ref with weak crossref match".into(),
            doi: None,
            arxiv_id: None,
            title: None,
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        // 100% unresolved → Fail.
        assert!(matches!(r.status, VerifierStatus::Fail), "got {:?}", r.status);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 0);
        assert_eq!(r.notes["unresolved"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn strip_version_drops_v_suffix() {
        assert_eq!(strip_version("2401.12345v2"), "2401.12345");
        assert_eq!(strip_version("2401.12345"), "2401.12345");
        assert_eq!(strip_version("math.AG/0301001"), "math.AG/0301001");
        assert_eq!(strip_version("vbogus"), "vbogus");
    }

    #[tokio::test]
    async fn passes_when_all_citations_resolve() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.good/doi"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let v = CitationVerifier::with_bases(format!("{}/works", server.uri()));
        let paper = paper_with(vec![Citation {
            raw: "Good".into(),
            doi: Some("10.good/doi".into()),
            arxiv_id: None,
            title: None,
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let r = v.verify(&json!({}), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass));
    }
}
