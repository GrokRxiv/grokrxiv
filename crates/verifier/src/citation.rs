//! Citation existence verifier.
//!
//! For every `Citation` on the paper carrying a DOI we issue a GET against
//! `https://api.crossref.org/works/{doi}` (metadata only — we never request a
//! PDF). arXiv-id-only citations are not probed by this rung: arXiv requests
//! must go through the [`grokrxiv-ingest`] rate-gate (single connection +
//! ≥3s spacing) and a separate `arxiv_id`-shape check is enough for MVP. A
//! future task may add a Semantic Scholar fallback for those entries.
//!
//! Results are memoised in an in-process map keyed by the lookup URL so a
//! batched verifier run on a paper with repeated citations only spends one
//! network round-trip per unique citation.
//!
//! NOTE: this rung must never request anything that could be construed as
//! serving PDFs or LaTeX source. Metadata lookups only.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{Verifier, VerifierContext};

/// Mark Fail if more than this fraction of citations are unresolved.
const FAIL_FRACTION: f32 = 0.30;

/// Citation verifier. Caches results across verify() calls within the process.
pub struct CitationVerifier {
    /// Base URL for Crossref `/works/{doi}` lookups.
    crossref_base: String,
    cache: Arc<Mutex<HashMap<String, bool>>>,
}

impl Default for CitationVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl CitationVerifier {
    /// Construct a verifier pointed at the real Crossref endpoint.
    pub fn new() -> Self {
        Self {
            crossref_base: "https://api.crossref.org/works".to_string(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with a custom Crossref base URL; intended for
    /// tests against `wiremock`.
    pub fn with_bases(crossref_base: impl Into<String>) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            cache: Arc::new(Mutex::new(HashMap::new())),
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
        let mut total: u32 = 0;
        let mut unresolved: Vec<String> = Vec::new();
        for c in &ctx.paper.bibliography {
            let mut ok = false;
            if let Some(doi) = &c.doi {
                total += 1;
                if self.resolve_doi(ctx.http, doi).await {
                    ok = true;
                }
            }
            if !ok {
                if let Some(arxiv_id) = &c.arxiv_id {
                    if c.doi.is_none() {
                        total += 1;
                    }
                    // Shape check only — never probe arXiv directly. See
                    // module-level comment.
                    if Self::arxiv_id_well_formed(arxiv_id) {
                        ok = true;
                    }
                }
            }
            if !ok && (c.doi.is_some() || c.arxiv_id.is_some()) {
                unresolved.push(c.raw.clone());
            }
        }

        if total == 0 {
            return VerifierResult {
                status: VerifierStatus::Pass,
                notes: json!({ "checked": 0 }),
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
            }),
        }
    }
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
