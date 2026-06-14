//! Citation existence verifier.
//!
//! Every citation gets a real existence check:
//!   - `Citation.doi`        → `GET {crossref_base}/{doi}` (metadata only).
//!   - `Citation.arxiv_id`   → batched `GET {arxiv_base}?id_list=ID1,ID2,...`
//!                             with explicit `max_results` for each page.
//!   - Plain refs (no DOI, no arxiv_id) → Crossref free-text bibliographic
//!                             query first, then OpenAlex, Semantic Scholar,
//!                             NASA ADS, INSPIRE-HEP, and zbMATH Open for
//!                             pre-DOI classics, then an optional URL-backed
//!                             Gemini-grounded adjudicator endpoint for residue.
//!
//! Results are memoised in-process by lookup URL so a paper with repeated
//! citations only spends one network round-trip per unique key. The verifier
//! deliberately stays metadata-only — never requests PDFs or LaTeX.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::{Verifier, VerifierContext};

/// Mark Fail if more than this fraction of citations are unresolved.
const FAIL_FRACTION: f32 = 0.30;

/// Minimum crossref `score` for a free-text bibliographic match to count as
/// resolved. Crossref's score ranges roughly 0-200 for a top hit; ~60 is the
/// rule-of-thumb floor where the result is meaningfully relevant.
const BIBLIOGRAPHIC_MATCH_SCORE_MIN: f64 = 60.0;
const ARXIV_ID_LIST_CHUNK_SIZE: usize = 100;
const BIBLIOGRAPHIC_PROVIDER_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BibliographicProviderKind {
    OpenAlex,
    SemanticScholar,
    Ads,
    InspireHep,
    ZbMath,
    GeminiGroundedEndpoint,
    GeminiGroundedApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BibliographicProvider {
    source: &'static str,
    base_url: String,
    kind: BibliographicProviderKind,
    model: Option<String>,
    api_key: Option<String>,
}

impl BibliographicProvider {
    fn new(
        source: &'static str,
        base_url: impl Into<String>,
        kind: BibliographicProviderKind,
    ) -> Self {
        Self {
            source,
            base_url: base_url.into(),
            kind,
            model: None,
            api_key: None,
        }
    }

    fn new_gemini_grounded_api(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            source: "gemini_grounded",
            base_url: base_url.into(),
            kind: BibliographicProviderKind::GeminiGroundedApi,
            model: Some(model.into()),
            api_key: Some(api_key.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CitationLookupStatus {
    Resolved,
    Retracted,
    Unresolved,
    Unverified,
    TransientUnknown,
    Malformed,
}

impl CitationLookupStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Retracted => "retracted",
            Self::Unresolved => "unresolved",
            Self::Unverified => "unverified",
            Self::TransientUnknown => "transient_unknown",
            Self::Malformed => "malformed",
        }
    }

    fn exists_value(self) -> serde_json::Value {
        match self {
            Self::Resolved => serde_json::Value::Bool(true),
            Self::Retracted | Self::Unresolved | Self::Malformed => serde_json::Value::Bool(false),
            Self::Unverified | Self::TransientUnknown => serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CitationLookup {
    status: CitationLookupStatus,
    resolved_doi: Option<String>,
    resolved_url: Option<String>,
    source: &'static str,
    reason: Option<String>,
}

impl CitationLookup {
    fn resolved(
        source: &'static str,
        resolved_doi: Option<String>,
        resolved_url: Option<String>,
    ) -> Self {
        Self {
            status: CitationLookupStatus::Resolved,
            resolved_doi,
            resolved_url,
            source,
            reason: None,
        }
    }

    fn resolved_with_reason(
        source: &'static str,
        resolved_doi: Option<String>,
        resolved_url: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: CitationLookupStatus::Resolved,
            resolved_doi,
            resolved_url,
            source,
            reason: Some(reason.into()),
        }
    }

    fn unresolved(source: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status: CitationLookupStatus::Unresolved,
            resolved_doi: None,
            resolved_url: None,
            source,
            reason: Some(reason.into()),
        }
    }

    fn retracted(
        source: &'static str,
        resolved_doi: Option<String>,
        resolved_url: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: CitationLookupStatus::Retracted,
            resolved_doi,
            resolved_url,
            source,
            reason: Some(reason.into()),
        }
    }

    fn unverified(source: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status: CitationLookupStatus::Unverified,
            resolved_doi: None,
            resolved_url: None,
            source,
            reason: Some(reason.into()),
        }
    }

    fn unknown(source: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status: CitationLookupStatus::TransientUnknown,
            resolved_doi: None,
            resolved_url: None,
            source,
            reason: Some(reason.into()),
        }
    }

    fn malformed(source: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status: CitationLookupStatus::Malformed,
            resolved_doi: None,
            resolved_url: None,
            source,
            reason: Some(reason.into()),
        }
    }
}

/// Citation verifier. Caches results across verify() calls within the process.
pub struct CitationVerifier {
    /// Base URL for Crossref `/works/{doi}` lookups.
    crossref_base: String,
    /// Base URL for arXiv id-list metadata queries (Atom feed).
    arxiv_base: String,
    /// Base URL for DOI resolver fallback checks.
    doi_resolver_base: String,
    /// Ordered bibliographic resolver waterfall after Crossref misses.
    bibliographic_providers: Vec<BibliographicProvider>,
    cache: Arc<Mutex<HashMap<String, CitationLookup>>>,
    /// Resolved bibliographic queries keyed by raw reference string.
    biblio_cache: Arc<Mutex<HashMap<String, CitationLookup>>>,
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
            doi_resolver_base: "https://doi.org".to_string(),
            bibliographic_providers: default_bibliographic_providers(),
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
            doi_resolver_base: "https://doi.org".to_string(),
            bibliographic_providers: Vec::new(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with both bases overridden — used by tests that
    /// also need to mock the arXiv endpoint.
    pub fn with_all_bases(crossref_base: impl Into<String>, arxiv_base: impl Into<String>) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            arxiv_base: arxiv_base.into(),
            doi_resolver_base: "https://doi.org".to_string(),
            bibliographic_providers: Vec::new(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with Crossref, arXiv, and DOI resolver bases overridden.
    pub fn with_all_bases_and_doi(
        crossref_base: impl Into<String>,
        arxiv_base: impl Into<String>,
        doi_resolver_base: impl Into<String>,
    ) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            arxiv_base: arxiv_base.into(),
            doi_resolver_base: doi_resolver_base.into(),
            bibliographic_providers: Vec::new(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with every network base overridden. This keeps the
    /// PR-54 resolver-waterfall tests hermetic while production uses the real
    /// provider defaults from [`CitationVerifier::new`].
    pub fn with_bibliographic_provider_bases(
        crossref_base: impl Into<String>,
        arxiv_base: impl Into<String>,
        doi_resolver_base: impl Into<String>,
        openalex_base: impl Into<String>,
        semantic_scholar_base: impl Into<String>,
        ads_base: impl Into<String>,
        inspire_hep_base: impl Into<String>,
        zbmath_base: impl Into<String>,
    ) -> Self {
        Self {
            crossref_base: crossref_base.into(),
            arxiv_base: arxiv_base.into(),
            doi_resolver_base: doi_resolver_base.into(),
            bibliographic_providers: vec![
                BibliographicProvider::new(
                    "openalex",
                    openalex_base,
                    BibliographicProviderKind::OpenAlex,
                ),
                BibliographicProvider::new(
                    "semantic_scholar",
                    semantic_scholar_base,
                    BibliographicProviderKind::SemanticScholar,
                ),
                BibliographicProvider::new("ads", ads_base, BibliographicProviderKind::Ads),
                BibliographicProvider::new(
                    "inspire_hep",
                    inspire_hep_base,
                    BibliographicProviderKind::InspireHep,
                ),
                BibliographicProvider::new(
                    "zbmath",
                    zbmath_base,
                    BibliographicProviderKind::ZbMath,
                ),
            ],
            cache: Arc::new(Mutex::new(HashMap::new())),
            biblio_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct a verifier with deterministic providers plus a final
    /// URL-evidence grounded resolver. Production enables the same provider
    /// with `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL`.
    pub fn with_bibliographic_and_grounded_provider_bases(
        crossref_base: impl Into<String>,
        arxiv_base: impl Into<String>,
        doi_resolver_base: impl Into<String>,
        openalex_base: impl Into<String>,
        semantic_scholar_base: impl Into<String>,
        ads_base: impl Into<String>,
        inspire_hep_base: impl Into<String>,
        zbmath_base: impl Into<String>,
        grounded_base: impl Into<String>,
    ) -> Self {
        let mut verifier = Self::with_bibliographic_provider_bases(
            crossref_base,
            arxiv_base,
            doi_resolver_base,
            openalex_base,
            semantic_scholar_base,
            ads_base,
            inspire_hep_base,
            zbmath_base,
        );
        verifier
            .bibliographic_providers
            .push(BibliographicProvider::new(
                "gemini_grounded",
                grounded_base,
                BibliographicProviderKind::GeminiGroundedEndpoint,
            ));
        verifier
    }

    /// Construct a verifier with deterministic providers plus a final
    /// app-local Gemini API grounded resolver. This mirrors the production env
    /// path while keeping tests hermetic via a mock Gemini base URL.
    pub fn with_bibliographic_and_local_gemini_grounded_provider_bases(
        crossref_base: impl Into<String>,
        arxiv_base: impl Into<String>,
        doi_resolver_base: impl Into<String>,
        openalex_base: impl Into<String>,
        semantic_scholar_base: impl Into<String>,
        ads_base: impl Into<String>,
        inspire_hep_base: impl Into<String>,
        zbmath_base: impl Into<String>,
        gemini_base: impl Into<String>,
        gemini_model: impl Into<String>,
        gemini_api_key: impl Into<String>,
    ) -> Self {
        let mut verifier = Self::with_bibliographic_provider_bases(
            crossref_base,
            arxiv_base,
            doi_resolver_base,
            openalex_base,
            semantic_scholar_base,
            ads_base,
            inspire_hep_base,
            zbmath_base,
        );
        verifier
            .bibliographic_providers
            .push(BibliographicProvider::new_gemini_grounded_api(
                gemini_base,
                gemini_model,
                gemini_api_key,
            ));
        verifier
    }

    async fn resolve_doi(&self, http: &reqwest::Client, doi: &str) -> CitationLookup {
        let doi = doi.trim();
        if doi.is_empty() {
            return CitationLookup::malformed("crossref", "empty DOI");
        }
        let url = format!("{}/{doi}", self.crossref_base);
        if let Some(v) = self.cache.lock().get(&url).cloned() {
            return v;
        }
        let lookup = match send_json_with_retry(http, &url).await {
            JsonLookup::Ok(body) => {
                if let Some(reason) = crossref_retraction_evidence(&body) {
                    CitationLookup::retracted(
                        "crossref_retraction",
                        Some(doi.to_string()),
                        Some(format!("https://doi.org/{doi}")),
                        reason,
                    )
                } else {
                    CitationLookup::resolved(
                        "crossref",
                        Some(doi.to_string()),
                        Some(format!("https://doi.org/{doi}")),
                    )
                }
            }
            JsonLookup::Unresolved(reason) => {
                self.resolve_doi_resolver(
                    http,
                    doi,
                    format!("crossref {reason}"),
                    reason.contains("status 400") || reason.contains("status 422"),
                )
                .await
            }
            JsonLookup::Transient(err) => CitationLookup::unknown("crossref", err),
        };
        if lookup.status != CitationLookupStatus::TransientUnknown {
            self.cache.lock().insert(url, lookup.clone());
        }
        lookup
    }

    async fn resolve_doi_resolver(
        &self,
        http: &reqwest::Client,
        doi: &str,
        crossref_reason: String,
        malformed_on_missing: bool,
    ) -> CitationLookup {
        let url = format!("{}/{doi}", self.doi_resolver_base.trim_end_matches('/'));
        let cache_key = format!("doi_resolver::{url}");
        if let Some(v) = self.cache.lock().get(&cache_key).cloned() {
            return v;
        }
        let lookup = match send_doi_with_retry(http, &url).await {
            HttpLookup::Ok(status) if status.is_success() || status.is_redirection() => {
                CitationLookup::resolved(
                    "doi_resolver",
                    Some(doi.to_string()),
                    Some(format!("https://doi.org/{doi}")),
                )
            }
            HttpLookup::Ok(status) if matches!(status.as_u16(), 400 | 404 | 410 | 422) => {
                let reason = format!("{crossref_reason}; DOI resolver status {status}");
                if malformed_on_missing {
                    CitationLookup::malformed("doi_resolver", reason)
                } else {
                    CitationLookup::unresolved("doi_resolver", reason)
                }
            }
            HttpLookup::Ok(status) => CitationLookup::unknown(
                "doi_resolver",
                format!("{crossref_reason}; DOI resolver status {status}"),
            ),
            HttpLookup::Err(err) => CitationLookup::unknown(
                "doi_resolver",
                format!("{crossref_reason}; DOI resolver error: {err}"),
            ),
        };
        if lookup.status != CitationLookupStatus::TransientUnknown {
            self.cache.lock().insert(cache_key, lookup.clone());
        }
        lookup
    }

    /// Batched arXiv existence check. Calls `{arxiv_base}?id_list=id1,id2,...`
    /// with explicit page sizes and returns one lookup result per requested
    /// id. arXiv
    /// returns an Atom feed; we parse it permissively by scanning for the
    /// `<id>http(s)://arxiv.org/abs/{id}</id>` lines.
    async fn resolve_arxiv_ids(
        &self,
        http: &reqwest::Client,
        ids: &[String],
    ) -> HashMap<String, CitationLookup> {
        if ids.is_empty() {
            return HashMap::new();
        }
        // De-dup + filter for in-cache + shape-validate before hitting the wire.
        let mut to_query: Vec<String> = Vec::new();
        let mut out: HashMap<String, CitationLookup> = HashMap::new();
        for id in ids {
            let cache_key = format!("arxiv::{id}");
            if let Some(cached) = self.cache.lock().get(&cache_key).cloned() {
                out.insert(id.clone(), cached);
                continue;
            }
            if !Self::arxiv_id_well_formed(id) {
                let malformed = CitationLookup::malformed("arxiv", "malformed arXiv id");
                self.cache.lock().insert(cache_key, malformed.clone());
                out.insert(id.clone(), malformed);
                continue;
            }
            to_query.push(strip_version(id).to_string());
        }
        if to_query.is_empty() {
            return out;
        }

        to_query.sort();
        to_query.dedup();
        for chunk in to_query.chunks(ARXIV_ID_LIST_CHUNK_SIZE) {
            let id_list = url_form_encode(&chunk.join(","));
            let url = format!(
                "{}?id_list={id_list}&start=0&max_results={}",
                self.arxiv_base,
                chunk.len()
            );
            let response = send_text_with_retry(http, &url).await;
            match response {
                TextLookup::Ok(body) => {
                    for q in chunk {
                        let needle_https = format!("arxiv.org/abs/{q}");
                        let lookup = if body.contains(&needle_https) {
                            CitationLookup::resolved(
                                "arxiv",
                                None,
                                Some(format!("https://arxiv.org/abs/{q}")),
                            )
                        } else {
                            CitationLookup::unresolved("arxiv", "not present in arXiv response")
                        };
                        self.cache
                            .lock()
                            .insert(format!("arxiv::{q}"), lookup.clone());
                        out.insert(q.clone(), lookup);
                    }
                }
                TextLookup::Transient(reason) => {
                    for q in chunk {
                        out.insert(q.clone(), CitationLookup::unknown("arxiv", reason.clone()));
                    }
                }
            }
        }
        // Also accept the versioned variants (`{id}v2`) when the caller asked
        // for one — arXiv resolves them to the same underlying entry.
        for original in ids {
            if out.contains_key(original) {
                continue;
            }
            if let Some(lookup) = out.get(strip_version(original)).cloned() {
                out.insert(original.clone(), lookup);
            }
        }
        out
    }

    /// Free-text bibliographic lookup for refs that carry neither a DOI nor an
    /// arxiv_id. Crossref runs first; weak/noisy Crossref hits flow into the
    /// app-local provider waterfall so pre-DOI classics can resolve through
    /// OpenAlex/Semantic Scholar/ADS/INSPIRE/zbMATH without losing partial
    /// per-reference evidence.
    async fn resolve_bibliographic(&self, http: &reqwest::Client, raw: &str) -> CitationLookup {
        if raw.trim().is_empty() {
            return CitationLookup::malformed("crossref_bibliographic", "empty bibliographic text");
        }
        if let Some(v) = self.biblio_cache.lock().get(raw).cloned() {
            return v;
        }
        let query_text = bibliographic_query_text(raw);
        let crossref_lookup = self.resolve_crossref_bibliographic(http, raw).await;
        if crossref_lookup.status == CitationLookupStatus::Resolved
            || self.bibliographic_providers.is_empty()
        {
            if crossref_lookup.status != CitationLookupStatus::TransientUnknown {
                self.biblio_cache
                    .lock()
                    .insert(raw.to_string(), crossref_lookup.clone());
            }
            return crossref_lookup;
        }

        let mut reasons: Vec<String> = Vec::new();
        let mut transient_count = 0usize;
        let mut non_transient_count = 0usize;
        record_bibliographic_attempt(
            &mut reasons,
            &mut transient_count,
            &mut non_transient_count,
            &crossref_lookup,
        );

        for provider in &self.bibliographic_providers {
            let lookup = self
                .resolve_provider_bibliographic(http, provider, &query_text)
                .await;
            if lookup.status == CitationLookupStatus::Resolved {
                self.biblio_cache
                    .lock()
                    .insert(raw.to_string(), lookup.clone());
                return lookup;
            }
            record_bibliographic_attempt(
                &mut reasons,
                &mut transient_count,
                &mut non_transient_count,
                &lookup,
            );
        }

        let provider_names = self
            .bibliographic_providers
            .iter()
            .map(|provider| provider.source)
            .collect::<Vec<_>>()
            .join(" -> ");
        let reason = format!(
            "not verified by resolver waterfall (Crossref -> {provider_names}); {}",
            reasons.join("; ")
        );
        let resolved = if non_transient_count == 0 && transient_count > 0 {
            CitationLookup::unknown("citation_waterfall", reason)
        } else {
            CitationLookup::unverified("citation_waterfall", reason)
        };
        if resolved.status != CitationLookupStatus::TransientUnknown {
            self.biblio_cache
                .lock()
                .insert(raw.to_string(), resolved.clone());
        }
        resolved
    }

    async fn resolve_crossref_bibliographic(
        &self,
        http: &reqwest::Client,
        raw: &str,
    ) -> CitationLookup {
        let url = format!("{}?rows=1&query.bibliographic=", self.crossref_base);
        let encoded = url_form_encode(raw);
        let full = format!("{url}{encoded}");
        match send_json_with_retry(http, &full).await {
            JsonLookup::Ok(v) => {
                if let Some(doi) = top_doi_if_scored(&v, BIBLIOGRAPHIC_MATCH_SCORE_MIN) {
                    CitationLookup::resolved(
                        "crossref_bibliographic",
                        Some(doi.clone()),
                        Some(format!("https://doi.org/{doi}")),
                    )
                } else {
                    CitationLookup::unverified(
                        "crossref_bibliographic",
                        "not verified by Crossref bibliographic search; no match above score threshold",
                    )
                }
            }
            JsonLookup::Unresolved(reason) => {
                CitationLookup::unresolved("crossref_bibliographic", reason)
            }
            JsonLookup::Transient(reason) => {
                CitationLookup::unknown("crossref_bibliographic", reason)
            }
        }
    }

    async fn resolve_provider_bibliographic(
        &self,
        http: &reqwest::Client,
        provider: &BibliographicProvider,
        query_text: &str,
    ) -> CitationLookup {
        let url = provider_query_url(provider, query_text);
        match send_provider_json_with_timeout(http, &url, provider, query_text).await {
            JsonLookup::Ok(v) => {
                if let Some(hit) = provider_hit_if_title_matches(&v, provider.kind, query_text) {
                    match (provider.kind, hit.url.clone()) {
                        (
                            BibliographicProviderKind::GeminiGroundedEndpoint
                            | BibliographicProviderKind::GeminiGroundedApi,
                            Some(url),
                        ) => CitationLookup::resolved_with_reason(
                            provider.source,
                            hit.doi,
                            Some(url.clone()),
                            format!("grounded URL evidence: {url}"),
                        ),
                        _ => CitationLookup::resolved(provider.source, hit.doi, hit.url),
                    }
                } else {
                    CitationLookup::unverified(
                        provider.source,
                        "no title match in provider response",
                    )
                }
            }
            JsonLookup::Unresolved(reason) => CitationLookup::unverified(provider.source, reason),
            JsonLookup::Transient(reason) => CitationLookup::unknown(provider.source, reason),
        }
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
        let Some(bibliography) = ctx.paper_bibliography() else {
            return VerifierResult {
                status: VerifierStatus::Warn,
                notes: json!({
                    "checked": 0,
                    "coverage_status": "unsupported_subject",
                    "reason": "Citation verification requires a paper subject with bibliography entries.",
                    "subject_kind": ctx.subject_kind.as_str(),
                    "entries": [],
                }),
            };
        };
        // Phase 1: batch the arXiv-id-only refs so we hit the arXiv API once.
        let arxiv_ids: Vec<String> = bibliography
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
        let mut retracted: Vec<String> = Vec::new();
        let mut unverified: Vec<String> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        let mut malformed: Vec<String> = Vec::new();
        let mut resolved_via_biblio: u32 = 0;
        let mut entries: Vec<serde_json::Value> = Vec::with_capacity(bibliography.len());
        for c in &bibliography {
            total += 1;
            let mut lookup: Option<CitationLookup> = None;

            if let Some(doi) = &c.doi {
                lookup = Some(self.resolve_doi(ctx.http, doi).await);
            }
            if !matches!(
                lookup.as_ref().map(|l| l.status),
                Some(CitationLookupStatus::Resolved | CitationLookupStatus::Retracted)
            ) {
                if let Some(arxiv_id) = &c.arxiv_id {
                    if let Some(arxiv_lookup) = arxiv_resolved.get(arxiv_id).cloned() {
                        if lookup.is_none()
                            || matches!(
                                arxiv_lookup.status,
                                CitationLookupStatus::Resolved
                                    | CitationLookupStatus::TransientUnknown
                            )
                        {
                            lookup = Some(arxiv_lookup);
                        }
                    }
                }
            }
            if lookup.is_none() && c.doi.is_none() && c.arxiv_id.is_none() {
                let bibliographic_text = c
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or(&c.raw);
                let biblio_lookup = self
                    .resolve_bibliographic(ctx.http, bibliographic_text)
                    .await;
                if matches!(biblio_lookup.status, CitationLookupStatus::Resolved) {
                    resolved_via_biblio += 1;
                }
                lookup = Some(biblio_lookup);
            }
            let lookup = lookup
                .unwrap_or_else(|| CitationLookup::unresolved("none", "no resolvable identifier"));
            match lookup.status {
                CitationLookupStatus::Resolved => {}
                CitationLookupStatus::Retracted => retracted.push(c.raw.clone()),
                CitationLookupStatus::Unresolved => unresolved.push(c.raw.clone()),
                CitationLookupStatus::Unverified => unverified.push(c.raw.clone()),
                CitationLookupStatus::TransientUnknown => unknown.push(c.raw.clone()),
                CitationLookupStatus::Malformed => malformed.push(c.raw.clone()),
            }
            let citation_key = citation_key_from_raw(&c.raw);
            let display_title = c.title.clone().or_else(|| bib_field(&c.raw, "title"));
            let display_year = bib_field(&c.raw, "year").or_else(|| bib_field(&c.raw, "date"));
            let display_author = bib_field(&c.raw, "author");
            let display_url = bib_field(&c.raw, "url");
            entries.push(json!({
                "raw": c.raw,
                "citation_key": citation_key,
                "title": display_title,
                "author": display_author,
                "year": display_year,
                "doi": c.doi.clone(),
                "arxiv_id": c.arxiv_id.clone(),
                "url": display_url,
                "exists": lookup.status.exists_value(),
                "status": lookup.status.as_str(),
                "resolved_doi": lookup.resolved_doi,
                "resolved_url": lookup.resolved_url,
                "source": lookup.source,
                "verified_via": lookup.source,
                "reason": lookup.reason,
            }));
        }

        if total == 0 {
            return VerifierResult {
                status: VerifierStatus::Fail,
                notes: json!({
                    "checked": 0,
                    "coverage_status": "not_checked",
                    "reason": "No extracted bibliography entries were available for external citation verification.",
                    "entries": [],
                }),
            };
        }
        let definitive_total = total
            .saturating_sub(unknown.len() as u32)
            .saturating_sub(unverified.len() as u32);
        let definitive_bad = unresolved.len() + malformed.len() + retracted.len();
        let frac = if definitive_total == 0 {
            0.0
        } else {
            definitive_bad as f32 / definitive_total as f32
        };
        let status = if !retracted.is_empty() {
            VerifierStatus::Fail
        } else if definitive_bad == 0 && unknown.is_empty() && unverified.is_empty() {
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
                "retracted": retracted,
                "unverified": unverified,
                "unknown": unknown,
                "malformed": malformed,
                "unresolved_fraction": frac,
                "resolved_via_bibliographic_query": resolved_via_biblio,
                "entries": entries,
            }),
        }
    }
}

enum HttpLookup {
    Ok(reqwest::StatusCode),
    Err(String),
}

enum TextLookup {
    Ok(String),
    Transient(String),
}

enum JsonLookup {
    Ok(serde_json::Value),
    Unresolved(String),
    Transient(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BibliographicHit {
    title: String,
    doi: Option<String>,
    url: Option<String>,
}

fn default_bibliographic_providers() -> Vec<BibliographicProvider> {
    let mut providers = vec![
        BibliographicProvider::new(
            "openalex",
            "https://api.openalex.org/works",
            BibliographicProviderKind::OpenAlex,
        ),
        BibliographicProvider::new(
            "semantic_scholar",
            "https://api.semanticscholar.org/graph/v1/paper/search",
            BibliographicProviderKind::SemanticScholar,
        ),
        BibliographicProvider::new(
            "ads",
            "https://api.adsabs.harvard.edu/v1/search/query",
            BibliographicProviderKind::Ads,
        ),
        BibliographicProvider::new(
            "inspire_hep",
            "https://inspirehep.net/api/literature",
            BibliographicProviderKind::InspireHep,
        ),
        BibliographicProvider::new(
            "zbmath",
            "https://api.zbmath.org/v1/document/_search",
            BibliographicProviderKind::ZbMath,
        ),
    ];
    if let Ok(base_url) = std::env::var("GROKRXIV_CITATION_GROUNDED_RESOLVER_URL") {
        let base_url = base_url.trim();
        if !base_url.is_empty() {
            providers.push(BibliographicProvider::new(
                "gemini_grounded",
                base_url.to_string(),
                BibliographicProviderKind::GeminiGroundedEndpoint,
            ));
        }
    } else if let Some(api_key) = gemini_grounded_api_key() {
        providers.push(BibliographicProvider::new_gemini_grounded_api(
            nonblank_env("GROKRXIV_CITATION_GROUNDED_GEMINI_BASE_URL")
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string()),
            nonblank_env("GROKRXIV_CITATION_GROUNDED_MODEL")
                .unwrap_or_else(|| "gemini-2.5-pro".to_string()),
            api_key,
        ));
    }
    providers
}

fn gemini_grounded_api_key() -> Option<String> {
    nonblank_env("GOOGLE_GENERATIVE_AI_API_KEY")
        .or_else(|| nonblank_env("GEMINI_API_KEY"))
        .or_else(|| nonblank_env("GOOGLE_API_KEY"))
}

fn record_bibliographic_attempt(
    reasons: &mut Vec<String>,
    transient_count: &mut usize,
    non_transient_count: &mut usize,
    lookup: &CitationLookup,
) {
    let reason = lookup
        .reason
        .as_deref()
        .unwrap_or_else(|| lookup.status.as_str());
    reasons.push(format!("{}: {reason}", lookup.source));
    match lookup.status {
        CitationLookupStatus::TransientUnknown => *transient_count += 1,
        CitationLookupStatus::Retracted
        | CitationLookupStatus::Unresolved
        | CitationLookupStatus::Unverified
        | CitationLookupStatus::Malformed => *non_transient_count += 1,
        CitationLookupStatus::Resolved => {}
    }
}

fn crossref_retraction_evidence(body: &serde_json::Value) -> Option<String> {
    let message = body.get("message").unwrap_or(body);
    let mut evidence: Vec<String> = Vec::new();

    for field in ["update-to", "updated-by"] {
        if let Some(items) = message.get(field).and_then(|v| v.as_array()) {
            for item in items {
                let update_type = scalar_field(item, "type").unwrap_or_default();
                let label = scalar_field(item, "label").unwrap_or_default();
                if update_type.eq_ignore_ascii_case("retraction")
                    || label.to_ascii_lowercase().contains("retraction")
                {
                    evidence.push(format_crossref_retraction_update(field, item));
                }
            }
        }
    }

    if let Some(relations) = message.get("relation").and_then(|v| v.as_object()) {
        for (relation, value) in relations {
            if !relation.to_ascii_lowercase().contains("retract") {
                continue;
            }
            let items: Vec<&serde_json::Value> = value
                .as_array()
                .map(|items| items.iter().collect())
                .unwrap_or_else(|| vec![value]);
            if items.is_empty() {
                evidence.push(format!("relation {relation}"));
            }
            for item in items {
                let id = scalar_field(item, "id")
                    .or_else(|| scalar_field(item, "DOI"))
                    .or_else(|| scalar_field(item, "doi"));
                let asserted_by = scalar_field(item, "asserted-by");
                let mut parts = vec![format!("relation {relation}")];
                if let Some(id) = id {
                    parts.push(format!("id={id}"));
                }
                if let Some(asserted_by) = asserted_by {
                    parts.push(format!("asserted_by={asserted_by}"));
                }
                evidence.push(parts.join(" "));
            }
        }
    }

    if evidence.is_empty() {
        let title = message
            .get("title")
            .and_then(first_string)
            .unwrap_or_default();
        if title
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("retracted:")
        {
            evidence.push("title marked RETRACTED".to_string());
        }
    }

    (!evidence.is_empty()).then(|| evidence.join("; "))
}

fn format_crossref_retraction_update(field: &str, item: &serde_json::Value) -> String {
    let mut parts = vec![format!("{field} type=retraction")];
    if let Some(doi) = scalar_field(item, "DOI").or_else(|| scalar_field(item, "doi")) {
        parts.push(format!("doi={doi}"));
    }
    if let Some(source) = scalar_field(item, "source") {
        parts.push(format!("source={source}"));
    }
    if let Some(record_id) = scalar_field(item, "record-id") {
        parts.push(format!("record_id={record_id}"));
    }
    parts.join(" ")
}

fn scalar_field(value: &serde_json::Value, key: &str) -> Option<String> {
    let value = value.get(key)?;
    value
        .as_str()
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .or_else(|| value.as_f64().map(|n| n.to_string()))
}

fn provider_query_url(provider: &BibliographicProvider, query_text: &str) -> String {
    let encoded = url_form_encode(query_text);
    match provider.kind {
        BibliographicProviderKind::OpenAlex => {
            format!("{}?search={encoded}&per-page=5", provider.base_url)
        }
        BibliographicProviderKind::SemanticScholar => format!(
            "{}?query={encoded}&limit=5&fields=title,externalIds,url,year,venue",
            provider.base_url
        ),
        BibliographicProviderKind::Ads => {
            format!(
                "{}?q={encoded}&fl=title,doi,bibcode,identifier&rows=5",
                provider.base_url
            )
        }
        BibliographicProviderKind::InspireHep => {
            format!(
                "{}?q={encoded}&size=5&fields=titles,dois,urls",
                provider.base_url
            )
        }
        BibliographicProviderKind::ZbMath => {
            format!(
                "{}?search_string={encoded}&results_per_page=5",
                provider.base_url
            )
        }
        BibliographicProviderKind::GeminiGroundedEndpoint => {
            format!("{}?query={encoded}", provider.base_url)
        }
        BibliographicProviderKind::GeminiGroundedApi => {
            let model = provider
                .model
                .as_deref()
                .unwrap_or("gemini-2.5-pro")
                .trim_start_matches("models/");
            format!(
                "{}/v1beta/models/{model}:generateContent",
                provider.base_url.trim_end_matches('/')
            )
        }
    }
}

fn bibliographic_query_text(raw: &str) -> String {
    bib_field(raw, "title").unwrap_or_else(|| clean_bib_text(raw))
}

fn provider_hit_if_title_matches(
    body: &serde_json::Value,
    kind: BibliographicProviderKind,
    query_text: &str,
) -> Option<BibliographicHit> {
    let hits = match kind {
        BibliographicProviderKind::OpenAlex => openalex_hits(body),
        BibliographicProviderKind::SemanticScholar => semantic_scholar_hits(body),
        BibliographicProviderKind::Ads => ads_hits(body),
        BibliographicProviderKind::InspireHep => inspire_hep_hits(body),
        BibliographicProviderKind::ZbMath => zbmath_hits(body),
        BibliographicProviderKind::GeminiGroundedEndpoint
        | BibliographicProviderKind::GeminiGroundedApi => gemini_grounded_hits(body),
    };
    hits.into_iter()
        .find(|hit| title_matches(query_text, &hit.title))
}

fn title_matches(query_text: &str, candidate: &str) -> bool {
    let query = normalize_title_text(query_text);
    let candidate = normalize_title_text(candidate);
    if query.is_empty() || candidate.is_empty() {
        return false;
    }
    if query == candidate || query.contains(&candidate) || candidate.contains(&query) {
        return true;
    }
    let query_tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|token| token.len() > 2)
        .collect();
    let candidate_tokens: Vec<&str> = candidate
        .split_whitespace()
        .filter(|token| token.len() > 2)
        .collect();
    if query_tokens.is_empty() || candidate_tokens.is_empty() {
        return false;
    }
    let matches = query_tokens
        .iter()
        .filter(|token| candidate_tokens.contains(token))
        .count();
    matches * 2 >= query_tokens.len().max(candidate_tokens.len())
}

fn normalize_title_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_space = true;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        match ch {
            '\u{00e4}' => {
                out.push_str("ae");
                last_space = false;
            }
            '\u{00f6}' => {
                out.push_str("oe");
                last_space = false;
            }
            '\u{00fc}' => {
                out.push_str("ue");
                last_space = false;
            }
            '\u{00df}' => {
                out.push_str("ss");
                last_space = false;
            }
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                last_space = false;
            }
            _ if !last_space => {
                out.push(' ');
                last_space = true;
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

fn first_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .find(|field| !field.is_empty())
                .map(str::to_string)
        })
}

fn first_http_string(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    first_string(value).filter(|field| field.starts_with("http"))
}

fn normalize_doi(raw: &str) -> Option<String> {
    let doi = raw
        .trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("https://dx.doi.org/")
        .trim_start_matches("http://dx.doi.org/")
        .trim_start_matches("doi:")
        .trim()
        .trim_end_matches('.');
    (doi.starts_with("10.") && doi.contains('/')).then(|| doi.to_string())
}

fn openalex_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    body.get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title =
                string_field(item, "display_name").or_else(|| string_field(item, "title"))?;
            let doi = string_field(item, "doi").and_then(|doi| normalize_doi(&doi));
            let url = item
                .get("primary_location")
                .and_then(|location| string_field(location, "landing_page_url"))
                .or_else(|| string_field(item, "id"));
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn semantic_scholar_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    body.get("data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = string_field(item, "title")?;
            let doi = item
                .get("externalIds")
                .and_then(|ids| string_field(ids, "DOI"))
                .and_then(|doi| normalize_doi(&doi));
            let url = string_field(item, "url");
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn ads_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    body.get("response")
        .and_then(|v| v.get("docs"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = first_string(item.get("title")?)?;
            let doi = item
                .get("doi")
                .and_then(first_string)
                .and_then(|doi| normalize_doi(&doi));
            let url = first_http_string(item.get("identifier")).or_else(|| {
                string_field(item, "bibcode")
                    .map(|bibcode| format!("https://ui.adsabs.harvard.edu/abs/{bibcode}/abstract"))
            });
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn inspire_hep_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    body.get("hits")
        .and_then(|v| v.get("hits"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            let metadata = hit.get("metadata").unwrap_or(hit);
            let title = metadata
                .get("titles")
                .and_then(|titles| titles.as_array())
                .and_then(|titles| titles.first())
                .and_then(|title| string_field(title, "title"))?;
            let doi = metadata
                .get("dois")
                .and_then(|dois| dois.as_array())
                .and_then(|dois| dois.first())
                .and_then(|doi| string_field(doi, "value"))
                .and_then(|doi| normalize_doi(&doi));
            let url = metadata
                .get("urls")
                .and_then(|urls| urls.as_array())
                .and_then(|urls| urls.first())
                .and_then(|url| string_field(url, "value"))
                .or_else(|| string_field(hit, "links"));
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn zbmath_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    body.get("result")
        .or_else(|| body.get("results"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = zbmath_title(item)?;
            let doi = string_field(item, "doi").and_then(|doi| normalize_doi(&doi));
            let url = string_field(item, "zbmath_url")
                .or_else(|| string_field(item, "url"))
                .or_else(|| string_field(item, "id"))
                .or_else(|| {
                    string_field(item, "zbl_id").map(|id| format!("https://zbmath.org/{id}"))
                });
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn zbmath_title(item: &serde_json::Value) -> Option<String> {
    string_field(item, "title")
        .or_else(|| item.get("title").and_then(first_string))
        .or_else(|| {
            item.get("title")
                .and_then(|title| string_field(title, "title"))
        })
}

fn gemini_grounded_hits(body: &serde_json::Value) -> Vec<BibliographicHit> {
    grounded_candidate_values(body)
        .into_iter()
        .filter_map(|candidate| {
            let verdict = string_field(&candidate, "verdict")
                .or_else(|| string_field(&candidate, "status"))
                .unwrap_or_default();
            if !matches!(
                verdict.to_ascii_lowercase().as_str(),
                "verified" | "resolved" | "exists"
            ) {
                return None;
            }
            let title = string_field(&candidate, "title")?;
            let evidence_url = grounded_evidence_urls(&candidate).into_iter().next()?;
            let doi = string_field(&candidate, "resolved_doi")
                .or_else(|| string_field(&candidate, "doi"))
                .and_then(|doi| normalize_doi(&doi));
            let url = first_http_string(candidate.get("resolved_url")).or(Some(evidence_url));
            Some(BibliographicHit { title, doi, url })
        })
        .collect()
}

fn grounded_candidate_values(body: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut candidates = Vec::new();
    for field in ["candidates", "results"] {
        if let Some(items) = body.get(field).and_then(|value| value.as_array()) {
            for item in items {
                if item.get("content").is_some() || item.get("groundingMetadata").is_some() {
                    candidates.extend(gemini_api_grounded_candidate_values(item));
                } else {
                    candidates.push(item.clone());
                }
            }
        }
    }
    if candidates.is_empty() {
        candidates.push(body.clone());
    }
    candidates
}

fn gemini_api_grounded_candidate_values(candidate: &serde_json::Value) -> Vec<serde_json::Value> {
    let evidence_urls = gemini_grounding_metadata_urls(candidate);
    let mut out = Vec::new();
    if let Some(parts) = candidate
        .get("content")
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
    {
        for text in parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        {
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) {
                inject_grounding_evidence_urls(&mut value, &evidence_urls);
                out.push(value);
            }
        }
    }
    out
}

fn gemini_grounding_metadata_urls(candidate: &serde_json::Value) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(chunks) = candidate
        .get("groundingMetadata")
        .and_then(|metadata| metadata.get("groundingChunks"))
        .and_then(|chunks| chunks.as_array())
    {
        for chunk in chunks {
            collect_http_strings(
                chunk
                    .get("web")
                    .and_then(|web| web.get("uri"))
                    .or_else(|| chunk.get("retrievedContext").and_then(|ctx| ctx.get("uri"))),
                &mut urls,
            );
        }
    }
    urls.sort();
    urls.dedup();
    urls
}

fn inject_grounding_evidence_urls(value: &mut serde_json::Value, urls: &[String]) {
    if urls.is_empty() {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            if !map.contains_key("evidence_urls") {
                map.insert("evidence_urls".to_string(), json!(urls));
            }
            if !map.contains_key("resolved_url") {
                map.insert("resolved_url".to_string(), json!(urls[0]));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                inject_grounding_evidence_urls(item, urls);
            }
        }
        _ => {}
    }
}

fn grounded_evidence_urls(candidate: &serde_json::Value) -> Vec<String> {
    let mut urls = Vec::new();
    collect_http_strings(candidate.get("evidence_urls"), &mut urls);
    collect_http_strings(candidate.get("urls"), &mut urls);
    collect_http_strings(candidate.get("evidence"), &mut urls);
    if let Some(quorum) = candidate.get("quorum").and_then(|value| value.as_array()) {
        for item in quorum {
            collect_http_strings(item.get("url"), &mut urls);
            collect_http_strings(item.get("resolved_url"), &mut urls);
            collect_http_strings(item.get("evidence_urls"), &mut urls);
            collect_http_strings(item.get("evidence"), &mut urls);
        }
    }
    urls.sort();
    urls.dedup();
    urls
}

fn collect_http_strings(value: Option<&serde_json::Value>, out: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    match value {
        serde_json::Value::String(text) => collect_http_substrings(text, out),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_http_strings(Some(item), out);
            }
        }
        serde_json::Value::Object(map) => {
            for key in ["url", "resolved_url", "source_url", "evidence_url"] {
                collect_http_strings(map.get(key), out);
            }
        }
        _ => {}
    }
}

fn collect_http_substrings(text: &str, out: &mut Vec<String>) {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        });
        if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
            out.push(cleaned.trim_end_matches('.').to_string());
        }
    }
}

async fn send_doi_with_retry(http: &reqwest::Client, url: &str) -> HttpLookup {
    let mut last_err: Option<String> = None;
    for attempt in 0..2 {
        match http
            .get(url)
            .header(
                "accept",
                "application/vnd.citationstyles.csl+json, application/json;q=0.9, */*;q=0.1",
            )
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                if is_transient_status(status) && attempt == 0 {
                    last_err = Some(format!("status {status}"));
                    continue;
                }
                return HttpLookup::Ok(status);
            }
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt == 0 {
                    continue;
                }
            }
        }
    }
    HttpLookup::Err(last_err.unwrap_or_else(|| "request failed".to_string()))
}

async fn send_text_with_retry(http: &reqwest::Client, url: &str) -> TextLookup {
    let mut last_err: Option<String> = None;
    for attempt in 0..2 {
        match http.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return match response.text().await {
                        Ok(body) => TextLookup::Ok(body),
                        Err(err) => TextLookup::Transient(err.to_string()),
                    };
                }
                if is_transient_status(status) && attempt == 0 {
                    last_err = Some(format!("status {status}"));
                    continue;
                }
                return if is_definitive_missing_status(status) {
                    TextLookup::Ok(String::new())
                } else {
                    TextLookup::Transient(format!("status {status}"))
                };
            }
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt == 0 {
                    continue;
                }
            }
        }
    }
    TextLookup::Transient(last_err.unwrap_or_else(|| "request failed".to_string()))
}

async fn send_json_with_retry(http: &reqwest::Client, url: &str) -> JsonLookup {
    let mut last_err: Option<String> = None;
    for attempt in 0..2 {
        match http.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return match response.json::<serde_json::Value>().await {
                        Ok(body) => JsonLookup::Ok(body),
                        Err(err) => JsonLookup::Transient(err.to_string()),
                    };
                }
                if is_transient_status(status) && attempt == 0 {
                    last_err = Some(format!("status {status}"));
                    continue;
                }
                return if is_definitive_missing_status(status) {
                    JsonLookup::Unresolved(format!("status {status}"))
                } else {
                    JsonLookup::Transient(format!("status {status}"))
                };
            }
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt == 0 {
                    continue;
                }
            }
        }
    }
    JsonLookup::Transient(last_err.unwrap_or_else(|| "request failed".to_string()))
}

async fn send_provider_json_with_timeout(
    http: &reqwest::Client,
    url: &str,
    provider: &BibliographicProvider,
    query_text: &str,
) -> JsonLookup {
    match tokio::time::timeout(
        Duration::from_secs(BIBLIOGRAPHIC_PROVIDER_TIMEOUT_SECS),
        send_provider_json_with_retry(http, url, provider, query_text),
    )
    .await
    {
        Ok(lookup) => lookup,
        Err(_) => JsonLookup::Transient(format!(
            "timeout after {BIBLIOGRAPHIC_PROVIDER_TIMEOUT_SECS}s"
        )),
    }
}

async fn send_provider_json_with_retry(
    http: &reqwest::Client,
    url: &str,
    provider: &BibliographicProvider,
    query_text: &str,
) -> JsonLookup {
    let mut last_err: Option<String> = None;
    for attempt in 0..2 {
        let request = provider_request(http, url, provider, query_text);
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return match response.json::<serde_json::Value>().await {
                        Ok(body) => JsonLookup::Ok(body),
                        Err(err) => JsonLookup::Transient(err.to_string()),
                    };
                }
                if is_transient_status(status) && attempt == 0 {
                    last_err = Some(format!("status {status}"));
                    continue;
                }
                return if is_definitive_missing_status(status) {
                    JsonLookup::Unresolved(format!("status {status}"))
                } else {
                    JsonLookup::Transient(format!("status {status}"))
                };
            }
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt == 0 {
                    continue;
                }
            }
        }
    }
    JsonLookup::Transient(last_err.unwrap_or_else(|| "request failed".to_string()))
}

fn provider_request(
    http: &reqwest::Client,
    url: &str,
    provider: &BibliographicProvider,
    query_text: &str,
) -> reqwest::RequestBuilder {
    let request = match provider.kind {
        BibliographicProviderKind::GeminiGroundedApi => http
            .post(url)
            .json(&gemini_grounded_request_body(query_text)),
        _ => http.get(url),
    };
    apply_provider_headers(request, provider)
}

fn gemini_grounded_request_body(query_text: &str) -> serde_json::Value {
    json!({
        "contents": [{
            "role": "user",
            "parts": [{
                "text": format!(
                    "Verify whether this bibliographic reference title exists: {query_text}\n\
    Return only JSON with fields verdict, title, resolved_doi, resolved_url, evidence_urls, explanation. \
    Use verdict=verified only when Google Search grounding provides URL evidence for the same title; otherwise use verdict=needs_review."
                )
            }]
        }],
        "tools": [{"google_search": {}}],
        "generationConfig": {
            "temperature": 0.0,
            "maxOutputTokens": 1024,
            "responseMimeType": "application/json",
            "thinkingConfig": {"thinkingBudget": 0}
        }
    })
}

fn apply_provider_headers(
    mut request: reqwest::RequestBuilder,
    provider: &BibliographicProvider,
) -> reqwest::RequestBuilder {
    match provider.kind {
        BibliographicProviderKind::SemanticScholar => {
            if let Some(token) = nonblank_env("SEMANTIC_SCHOLAR_API_KEY") {
                request = request.header("x-api-key", token);
            }
        }
        BibliographicProviderKind::Ads => {
            if let Some(token) =
                nonblank_env("NASA_ADS_API_TOKEN").or_else(|| nonblank_env("ADS_API_TOKEN"))
            {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
        }
        BibliographicProviderKind::GeminiGroundedApi => {
            if let Some(token) = provider
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                request = request.header("x-goog-api-key", token);
            }
        }
        _ => {}
    }
    request
}

fn nonblank_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_transient_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_definitive_missing_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 404 | 410 | 422)
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
    let item = body.get("message")?.get("items")?.as_array()?.first()?;
    let score = item.get("score")?.as_f64()?;
    if score < min_score {
        return None;
    }
    item.get("DOI")?.as_str().map(str::to_string)
}

fn citation_key_from_raw(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('@') {
        if let Some((_, after_open)) = rest.split_once('{') {
            let key = after_open.split(',').next().unwrap_or_default().trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    if let Some((key, _)) = trimmed.split_once(':') {
        let key = key.trim();
        if !key.is_empty()
            && key.len() <= 96
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
        {
            return Some(key.to_string());
        }
    }
    None
}

fn bib_field(raw: &str, field: &str) -> Option<String> {
    let idx = raw
        .find(&format!("{field} ="))
        .or_else(|| raw.find(&format!("{field}=")))?;
    let after_equals = raw[idx..].split_once('=')?.1.trim_start();
    let (value, _) = parse_bib_value(after_equals)?;
    let cleaned = clean_bib_text(&value);
    (!cleaned.is_empty()).then_some(cleaned)
}

fn parse_bib_value(input: &str) -> Option<(String, usize)> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first == '{' {
        let mut depth = 1usize;
        let start = 1usize;
        for (idx, ch) in chars {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some((input[start..idx].to_string(), idx + ch.len_utf8()));
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if first == '"' {
        let start = 1usize;
        for (idx, ch) in chars {
            if ch == '"' {
                return Some((input[start..idx].to_string(), idx + ch.len_utf8()));
            }
        }
        return None;
    }
    let value = input
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((value, input.find(',').unwrap_or(input.len())))
}

fn clean_bib_text(value: &str) -> String {
    value
        .replace("{{", "")
        .replace("}}", "")
        .replace('{', "")
        .replace('}', "")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::{Citation, PaperExtract};
    use std::sync::{Mutex as StdMutex, OnceLock};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }

        fn clear(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

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

    fn classic_ref(key: &str, title: &str, year: u16) -> Citation {
        Citation {
            raw: format!("@article{{{key}, title = {{{title}}}, year = {{{year}}}}}"),
            doi: None,
            arxiv_id: None,
            title: Some(title.to_string()),
        }
    }

    #[tokio::test]
    async fn no_citations_fail_as_not_checked() {
        let v = CitationVerifier::new();
        let paper = paper_with(vec![]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Fail));
        assert_eq!(r.notes["checked"], 0);
        assert_eq!(r.notes["coverage_status"], "not_checked");
    }

    #[tokio::test]
    async fn warns_when_a_citation_is_unresolved() {
        let server = MockServer::start().await;
        // Resolved DOI.
        Mock::given(method("GET"))
            .and(path("/works/10.good/doi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "DOI": "10.good/doi",
                    "URL": "https://doi.org/10.good/doi"
                }
            })))
            .mount(&server)
            .await;
        // Unresolved DOI.
        Mock::given(method("GET"))
            .and(path("/works/10.bad/doi"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/doi/10.bad/doi"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_all_bases_and_doi(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
        );
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
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        // 50% unresolved > 30% → Fail.
        assert!(matches!(r.status, VerifierStatus::Fail));
        assert_eq!(r.notes["checked"], 2);
    }

    #[tokio::test]
    async fn doi_crossref_miss_falls_back_to_doi_resolver() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.datacite/example"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/doi/10.datacite/example"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_all_bases_and_doi(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
        );
        let paper = paper_with(vec![Citation {
            raw: "Repository DOI".into(),
            doi: Some("10.datacite/example".into()),
            arxiv_id: None,
            title: Some("Repository DOI".into()),
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["entries"][0]["status"], "resolved");
        assert_eq!(r.notes["entries"][0]["source"], "doi_resolver");
        assert_eq!(
            r.notes["entries"][0]["resolved_url"],
            "https://doi.org/10.datacite/example"
        );
    }

    #[tokio::test]
    async fn doi_crossref_retraction_metadata_marks_gate_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.retracted/example"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "DOI": "10.retracted/example",
                    "URL": "https://doi.org/10.retracted/example",
                    "update-to": [{
                        "DOI": "10.notice/retraction",
                        "type": "retraction",
                        "source": "publisher",
                        "label": "Retraction"
                    }],
                    "updated-by": [{
                        "DOI": "10.retractionwatch/record",
                        "type": "retraction",
                        "source": "retraction-watch",
                        "label": "Retraction",
                        "record-id": "44124"
                    }]
                }
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_all_bases_and_doi(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
        );
        let paper = paper_with(vec![Citation {
            raw: "Retracted paper".into(),
            doi: Some("10.retracted/example".into()),
            arxiv_id: None,
            title: Some("Retracted paper".into()),
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Fail), "{:?}", r);
        assert_eq!(r.notes["retracted"].as_array().unwrap().len(), 1);
        assert_eq!(r.notes["entries"][0]["status"], "retracted");
        assert_eq!(r.notes["entries"][0]["exists"], false);
        assert_eq!(r.notes["entries"][0]["source"], "crossref_retraction");
        let reason = r.notes["entries"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("10.notice/retraction"), "{reason}");
        assert!(reason.contains("retraction-watch"), "{reason}");
    }

    #[test]
    fn arxiv_id_shape_checker() {
        assert!(CitationVerifier::arxiv_id_well_formed("2605.12484"));
        assert!(CitationVerifier::arxiv_id_well_formed("2401.12345v2"));
        assert!(CitationVerifier::arxiv_id_well_formed("math.AG/0301001"));
        assert!(!CitationVerifier::arxiv_id_well_formed("not-an-arxiv-id"));
        assert!(!CitationVerifier::arxiv_id_well_formed("2605.12"));
    }

    #[test]
    fn citation_key_from_raw_preserves_plus() {
        assert_eq!(
            citation_key_from_raw("HofmannMorris+2023: The Structure of Compact Groups").as_deref(),
            Some("HofmannMorris+2023")
        );
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
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        // 1 / 3 = 33% unresolved → Fail (threshold is 30%).
        assert!(
            matches!(r.status, VerifierStatus::Fail),
            "got {:?}",
            r.status
        );
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
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        assert!(
            matches!(r.status, VerifierStatus::Pass),
            "got {:?}",
            r.status
        );
        assert_eq!(r.notes["checked"], 1);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 1);
    }

    #[tokio::test]
    async fn bibliographic_query_below_threshold_is_unverified_not_missing() {
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
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        // Crossref bibliographic search is not a definitive existence proof.
        // A weak/noisy top hit should ask for human review, not mark the
        // reference as missing or fail the gate by itself.
        assert!(
            matches!(r.status, VerifierStatus::Warn),
            "got {:?}",
            r.status
        );
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 0);
        assert_eq!(r.notes["unresolved"].as_array().unwrap().len(), 0);
        assert_eq!(r.notes["unverified"].as_array().unwrap().len(), 1);
        assert_eq!(r.notes["entries"][0]["status"], "unverified");
        assert_eq!(r.notes["entries"][0]["exists"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn bibliographic_waterfall_prefers_structured_title_over_raw_label() {
        let server = MockServer::start().await;
        let title = "On Manifolds with an Affine Connection and the Theory of General Relativity";
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .and(query_param("search", title))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "display_name": title,
                    "doi": null,
                    "id": "https://openalex.org/W583184355",
                    "primary_location": {
                        "landing_page_url": "http://cds.cern.ch/record/108877"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_structured_search", server.uri()),
        );
        let paper = paper_with(vec![Citation {
            raw: "Cartan:1986: On Manifolds with an Affine Connection and the Theory of General Relativity, 1986".into(),
            doi: None,
            arxiv_id: None,
            title: Some(title.to_string()),
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 1);
        assert_eq!(r.notes["entries"][0]["status"], "resolved");
        assert_eq!(r.notes["entries"][0]["verified_via"], "openalex");
    }

    #[tokio::test]
    async fn zbmath_search_string_resolves_object_title_results() {
        let server = MockServer::start().await;
        let title =
            "Zur Infinitesimalgeometrie: Einordnung der projektiven und der konformen Auffassung";
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/semantic/graph/v1/paper/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ads/v1/search/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {"docs": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/inspire/api/literature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {"hits": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/zbmath/v1/document/_search"))
            .and(query_param("search_string", title))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "title": {
                        "title": "Zur Infinitesimalgeometrie: Einordnung der projektiven und der konformen Auffassung."
                    },
                    "identifier": "48.0844.04",
                    "zbmath_url": "https://zbmath.org/2603060",
                    "year": "1921"
                }]
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_search", server.uri()),
        );
        let paper = paper_with(vec![classic_ref("weyl1921", title, 1921)]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 1);
        assert_eq!(r.notes["entries"][0]["status"], "resolved");
        assert_eq!(r.notes["entries"][0]["source"], "zbmath");
        assert_eq!(
            r.notes["entries"][0]["resolved_url"],
            "https://zbmath.org/2603060"
        );
    }

    #[tokio::test]
    async fn bibliographic_waterfall_resolves_pr54_classics_and_keeps_partial_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/semantic/graph/v1/paper/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ads/v1/search/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "docs": [
                        {
                            "title": ["Relativitaetstheorie und Mathematik"],
                            "doi": ["10.ads/cartan"],
                            "bibcode": "1922JRAM..146...16C",
                            "identifier": ["https://ui.adsabs.harvard.edu/abs/1922JRAM..146...16C/abstract"]
                        },
                        {
                            "title": ["Survey of General Relativity Theory"],
                            "bibcode": "1962ctgr.book.....E",
                            "identifier": ["https://ui.adsabs.harvard.edu/abs/1962ctgr.book.....E/abstract"]
                        },
                        {
                            "title": ["Galilei and Lorentz Structures on Space-Time"],
                            "doi": ["10.ads/kunzle"],
                            "bibcode": "1972AnPhy..72..445K"
                        }
                    ]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/inspire/api/literature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {
                    "hits": []
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/zbmath/v1/document/_structured_search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [
                    {
                        "title": "Foundations and Current Problems of General Relativity",
                        "doi": "10.zbmath/trautman",
                        "url": "https://zbmath.org/trautman"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_structured_search", server.uri()),
        );
        let paper = paper_with(vec![
            classic_ref("cartan1922", "Relativitaetstheorie und Mathematik", 1922),
            classic_ref("ehlers1962", "Survey of General Relativity Theory", 1962),
            classic_ref(
                "kunzle1972",
                "Galilei and Lorentz Structures on Space-Time",
                1972,
            ),
            classic_ref(
                "trautman1966",
                "Foundations and Current Problems of General Relativity",
                1966,
            ),
            classic_ref("reichenbach1928", "Philosophie der Raum-Zeit-Lehre", 1928),
            classic_ref("unknown1901", "A deliberately unresolved classic", 1901),
        ]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Warn), "{:?}", r);
        assert_eq!(r.notes["checked"], 6);
        assert_eq!(r.notes["entries"].as_array().unwrap().len(), 6);
        assert_eq!(r.notes["unresolved"].as_array().unwrap().len(), 0);
        assert_eq!(r.notes["unverified"].as_array().unwrap().len(), 2);
        let sources: Vec<&str> = r.notes["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["status"] == "resolved")
            .filter_map(|entry| entry["verified_via"].as_str())
            .collect();
        assert!(sources.contains(&"ads"), "sources={sources:?}");
        assert!(sources.contains(&"zbmath"), "sources={sources:?}");
    }

    #[tokio::test]
    async fn provider_requests_include_semantic_scholar_and_ads_auth_headers() {
        let _lock = ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("env lock");
        let _semantic_key = EnvVarGuard::set("SEMANTIC_SCHOLAR_API_KEY", "s2-test-key");
        let _ads_key = EnvVarGuard::set("NASA_ADS_API_TOKEN", "ads-test-key");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/semantic/graph/v1/paper/search"))
            .and(header("x-api-key", "s2-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ads/v1/search/query"))
            .and(header("Authorization", "Bearer ads-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "docs": [{
                        "title": ["Philosophie der Raum-Zeit-Lehre"],
                        "bibcode": "1928przl.book.....R",
                        "identifier": ["https://ui.adsabs.harvard.edu/abs/1928przl.book.....R/abstract"]
                    }]
                }
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_structured_search", server.uri()),
        );
        let paper = paper_with(vec![classic_ref(
            "reichenbach1928",
            "Philosophie der Raum-Zeit-Lehre",
            1928,
        )]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["entries"][0]["source"], "ads");
    }

    #[tokio::test]
    async fn grounded_fallback_resolves_residue_with_url_evidence() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/semantic/graph/v1/paper/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ads/v1/search/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {"docs": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/inspire/api/literature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {"hits": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/zbmath/v1/document/_structured_search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/grounded/citation"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "verdict": "verified",
                "title": "Philosophie der Raum-Zeit-Lehre",
                "resolved_url": "https://archive.org/details/philosophiederra00reic",
                "evidence_urls": ["https://archive.org/details/philosophiederra00reic"],
                "evidence": [
                    "Gemini grounded search found a digitized catalogue record with the matching title."
                ],
                "quorum": [
                    {
                        "provider": "gemini_grounded",
                        "verdict": "verified",
                        "url": "https://archive.org/details/philosophiederra00reic"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_and_grounded_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_structured_search", server.uri()),
            format!("{}/grounded/citation", server.uri()),
        );
        let paper = paper_with(vec![classic_ref(
            "reichenbach1928",
            "Philosophie der Raum-Zeit-Lehre",
            1928,
        )]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["checked"], 1);
        assert_eq!(r.notes["unverified"].as_array().unwrap().len(), 0);
        assert_eq!(r.notes["resolved_via_bibliographic_query"], 1);
        assert_eq!(r.notes["entries"][0]["status"], "resolved");
        assert_eq!(r.notes["entries"][0]["source"], "gemini_grounded");
        assert_eq!(
            r.notes["entries"][0]["resolved_url"],
            "https://archive.org/details/philosophiederra00reic"
        );
        let reason = r.notes["entries"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("https://archive.org/details/philosophiederra00reic"));
    }

    #[test]
    fn default_providers_include_local_gemini_api_when_key_is_configured() {
        let _lock = ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("env lock");
        let _endpoint = EnvVarGuard::clear("GROKRXIV_CITATION_GROUNDED_RESOLVER_URL");
        let _key = EnvVarGuard::set("GEMINI_API_KEY", "env-gemini-key");
        let _base = EnvVarGuard::set(
            "GROKRXIV_CITATION_GROUNDED_GEMINI_BASE_URL",
            "http://gemini.test",
        );
        let _model = EnvVarGuard::set("GROKRXIV_CITATION_GROUNDED_MODEL", "gemini-2.5-pro");

        let providers = default_bibliographic_providers();

        let provider = providers
            .iter()
            .find(|provider| provider.kind == BibliographicProviderKind::GeminiGroundedApi)
            .expect("local gemini grounded provider");
        assert_eq!(provider.source, "gemini_grounded");
        assert_eq!(provider.base_url, "http://gemini.test");
        assert_eq!(provider.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(provider.api_key.as_deref(), Some("env-gemini-key"));
    }

    #[tokio::test]
    async fn local_gemini_grounded_api_resolves_residue_with_grounding_metadata() {
        let _lock = ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("env lock");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "items": [{
                        "DOI": "10.weak/crossref",
                        "score": 8.0,
                        "title": ["Weak unrelated match"],
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openalex/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/semantic/graph/v1/paper/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ads/v1/search/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {"docs": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/inspire/api/literature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {"hits": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/zbmath/v1/document/_structured_search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-2.5-pro:generateContent"))
            .and(header("x-goog-api-key", "test-gemini-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "{\"verdict\":\"verified\",\"title\":\"Philosophie der Raum-Zeit-Lehre\",\"resolved_url\":\"https://archive.org/details/philosophiederra00reic\",\"evidence_urls\":[\"https://archive.org/details/philosophiederra00reic\"]}"
                        }]
                    },
                    "groundingMetadata": {
                        "webSearchQueries": ["Philosophie der Raum-Zeit-Lehre Reichenbach"],
                        "groundingChunks": [{
                            "web": {
                                "uri": "https://archive.org/details/philosophiederra00reic",
                                "title": "Philosophie der Raum-Zeit-Lehre"
                            }
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bibliographic_and_local_gemini_grounded_provider_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
            format!("{}/doi", server.uri()),
            format!("{}/openalex/works", server.uri()),
            format!("{}/semantic/graph/v1/paper/search", server.uri()),
            format!("{}/ads/v1/search/query", server.uri()),
            format!("{}/inspire/api/literature", server.uri()),
            format!("{}/zbmath/v1/document/_structured_search", server.uri()),
            server.uri(),
            "gemini-2.5-pro",
            "test-gemini-key",
        );
        let paper = paper_with(vec![classic_ref(
            "reichenbach1928",
            "Philosophie der Raum-Zeit-Lehre",
            1928,
        )]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        assert_eq!(r.notes["entries"][0]["status"], "resolved");
        assert_eq!(r.notes["entries"][0]["source"], "gemini_grounded");
        assert_eq!(
            r.notes["entries"][0]["resolved_url"],
            "https://archive.org/details/philosophiederra00reic"
        );
        let requests = server.received_requests().await.unwrap();
        let gemini_request = requests
            .iter()
            .find(|request| request.url.path().contains(":generateContent"))
            .expect("gemini request");
        let body: serde_json::Value = serde_json::from_slice(&gemini_request.body).unwrap();
        assert_eq!(body["tools"][0]["google_search"], json!({}));
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
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": {
                    "DOI": "10.good/doi",
                    "URL": "https://doi.org/10.good/doi"
                }
            })))
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
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({}), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass));
    }

    #[tokio::test]
    async fn transient_crossref_errors_are_unknown_not_unresolved() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.transient/doi"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;

        let v = CitationVerifier::with_bases(format!("{}/works", server.uri()));
        let paper = paper_with(vec![Citation {
            raw: "Temporarily unavailable".into(),
            doi: Some("10.transient/doi".into()),
            arxiv_id: None,
            title: None,
        }]);
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Warn), "{:?}", r);
        assert_eq!(r.notes["unresolved"].as_array().unwrap().len(), 0);
        assert_eq!(r.notes["unknown"].as_array().unwrap().len(), 1);
        assert_eq!(r.notes["unresolved_fraction"], 0.0);
        assert_eq!(r.notes["entries"][0]["status"], "transient_unknown");
        assert_eq!(r.notes["entries"][0]["exists"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn arxiv_lookup_sets_explicit_max_results_for_more_than_default_page() {
        let server = MockServer::start().await;
        let ids: Vec<String> = (0..12).map(|i| format!("2605.{:05}", i)).collect();
        let atom = ids
            .iter()
            .map(|id| format!("<entry><id>http://arxiv.org/abs/{id}</id></entry>"))
            .collect::<String>();
        Mock::given(method("GET"))
            .and(path("/api/query"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!(r#"<?xml version="1.0"?><feed>{atom}</feed>"#)),
            )
            .mount(&server)
            .await;

        let v = CitationVerifier::with_all_bases(
            format!("{}/works", server.uri()),
            format!("{}/api/query", server.uri()),
        );
        let paper = paper_with(
            ids.iter()
                .map(|id| Citation {
                    raw: id.clone(),
                    doi: None,
                    arxiv_id: Some(id.clone()),
                    title: None,
                })
                .collect(),
        );
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let r = v.verify(&json!({}), &ctx).await;

        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let query = requests[0].url.query().unwrap_or("");
        assert!(query.contains("max_results=12"), "{query}");
        assert!(query.contains("id_list=2605.00000%2C2605.00001"), "{query}");
    }
}
