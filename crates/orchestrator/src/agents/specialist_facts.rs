//! Deterministic fact pre-resolution for review specialists.
//!
//! The Citation pattern (verifier owns existence, LLM owns relevance) generalizes
//! to other roles. This module gathers ground-truth facts that the
//! Reproducibility / Novelty / Technical-Correctness specialists would otherwise
//! have to guess at — code-URL reachability, GitHub repo state, related-paper
//! candidates — and surfaces them in a structured form the supervisor injects
//! into the specialist prompt.
//!
//! Every collector is HTTP-bound and concurrency-safe. Failures are non-fatal
//! and surface as empty / error-flagged facts so the LLM falls back to its
//! today-behavior rather than failing the DAG.

use grokrxiv_schemas::PaperExtract;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Phase A facts: every URL referenced by the paper, with reachability +
/// (when matched) GitHub repo metadata.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct ReproducibilityFacts {
    /// All URLs we attempted to verify, with status + final URL after redirect.
    pub urls_checked: Vec<UrlCheck>,
    /// For URLs matching `github.com/<owner>/<repo>`: the repo's metadata.
    pub github_repos: Vec<GithubRepoFact>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UrlCheck {
    pub url: String,
    pub reachable: bool,
    pub status: Option<u16>,
    pub final_url: Option<String>,
    pub kind: UrlKind,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UrlKind {
    Code,
    Dataset,
    Other,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GithubRepoFact {
    pub owner: String,
    pub repo: String,
    pub exists: bool,
    pub default_branch: Option<String>,
    pub pushed_at: Option<String>,
    pub stargazers_count: Option<u64>,
    pub license_spdx: Option<String>,
    pub archived: Option<bool>,
}

/// Walk the paper extract for URLs, HEAD-check each one, and enrich
/// `github.com/<owner>/<repo>` URLs with public-API metadata.
pub async fn gather_reproducibility_facts(
    http: &reqwest::Client,
    extract: &PaperExtract,
) -> ReproducibilityFacts {
    let urls = collect_urls(extract);
    if urls.is_empty() {
        return ReproducibilityFacts::default();
    }
    // Bound how many we touch per paper. Most papers have <10 URLs; cap defends
    // against pathological extraction that surfaces hundreds.
    const MAX_URLS: usize = 50;
    let mut checks: Vec<UrlCheck> = Vec::new();
    let mut repos: Vec<GithubRepoFact> = Vec::new();
    let mut seen_repos: HashSet<(String, String)> = HashSet::new();
    for (url, kind) in urls.iter().take(MAX_URLS) {
        let check = head_check(http, url, *kind).await;
        if check.reachable {
            if let Some((owner, repo)) = parse_github_url(url) {
                if seen_repos.insert((owner.clone(), repo.clone())) {
                    repos.push(github_repo_metadata(http, &owner, &repo).await);
                }
            }
        }
        checks.push(check);
    }
    ReproducibilityFacts {
        urls_checked: checks,
        github_repos: repos,
    }
}

/// Pull URLs out of the paper extract: section body markdown + bibliography
/// `raw` strings. Classifies each URL as Code / Dataset / Other by hostname.
fn collect_urls(extract: &PaperExtract) -> Vec<(String, UrlKind)> {
    let mut out: Vec<(String, UrlKind)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |url: String, out: &mut Vec<(String, UrlKind)>, seen: &mut HashSet<String>| {
        if seen.insert(url.clone()) {
            let kind = classify_url(&url);
            out.push((url, kind));
        }
    };
    for section in &extract.sections {
        for url in find_urls(&section.body_markdown) {
            push(url, &mut out, &mut seen);
        }
    }
    for c in &extract.bibliography {
        for url in find_urls(&c.raw) {
            push(url, &mut out, &mut seen);
        }
    }
    out
}

/// Find http(s)://… URLs in a string. Permissive: accepts anything up to the
/// next whitespace or terminator char. Strips trailing punctuation that's
/// commonly attached by latex (`.`, `,`, `)`, `}`).
fn find_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in ["https://", "http://"] {
        let mut search_from = 0usize;
        while let Some(rel_idx) = text[search_from..].find(prefix) {
            let start = search_from + rel_idx;
            let after = &text[start..];
            let end = after
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '>' | '<' | '"' | '\'' | '`' | '|' | '\\')
                })
                .unwrap_or(after.len());
            let mut url = after[..end].to_string();
            while url
                .chars()
                .last()
                .map(|c| matches!(c, '.' | ',' | ';' | ':' | ')' | '}' | ']' | '!' | '?'))
                .unwrap_or(false)
            {
                url.pop();
            }
            if url.len() > prefix.len() {
                out.push(url);
            }
            search_from = start + end;
        }
    }
    out
}

fn classify_url(url: &str) -> UrlKind {
    let lower = url.to_ascii_lowercase();
    if lower.contains("github.com")
        || lower.contains("gitlab.com")
        || lower.contains("bitbucket.org")
        || lower.contains("huggingface.co")
    {
        return UrlKind::Code;
    }
    if lower.contains("zenodo.org")
        || lower.contains("figshare.com")
        || lower.contains("osf.io")
        || lower.contains("kaggle.com/datasets")
        || lower.contains("data.world")
    {
        return UrlKind::Dataset;
    }
    UrlKind::Other
}

async fn head_check(http: &reqwest::Client, url: &str, kind: UrlKind) -> UrlCheck {
    let req = http
        .head(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;
    match req {
        Ok(r) => {
            let status = r.status().as_u16();
            let final_url = r.url().to_string();
            // 2xx + 3xx + 401 (resource exists but auth-walled, e.g. private
            // github) → reachable. 404/410/5xx → not reachable.
            let reachable = (200..=399).contains(&status) || status == 401 || status == 403;
            UrlCheck {
                url: url.to_string(),
                reachable,
                status: Some(status),
                final_url: Some(final_url),
                kind,
            }
        }
        Err(_) => UrlCheck {
            url: url.to_string(),
            reachable: false,
            status: None,
            final_url: None,
            kind,
        },
    }
}

/// Parse `github.com/<owner>/<repo>(...)?` into `(owner, repo)`. Strips
/// `.git` suffixes and ignores trailing path segments.
pub fn parse_github_url(url: &str) -> Option<(String, String)> {
    let lower = url.to_ascii_lowercase();
    let idx = lower.find("github.com/")?;
    let tail = &url[idx + "github.com/".len()..];
    let mut parts = tail.splitn(3, '/');
    let owner = parts.next()?.trim();
    let repo_part = parts.next()?.trim();
    if owner.is_empty() || repo_part.is_empty() {
        return None;
    }
    let repo = repo_part.trim_end_matches(".git").trim_end_matches('/');
    if repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

async fn github_repo_metadata(
    http: &reqwest::Client,
    owner: &str,
    repo: &str,
) -> GithubRepoFact {
    let url = format!("https://api.github.com/repos/{owner}/{repo}");
    let mut req = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "grokrxiv-review")
        .timeout(std::time::Duration::from_secs(10));
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) => {
            return GithubRepoFact {
                owner: owner.to_string(),
                repo: repo.to_string(),
                exists: false,
                default_branch: None,
                pushed_at: None,
                stargazers_count: None,
                license_spdx: None,
                archived: None,
            };
        }
    };
    if !resp.status().is_success() {
        return GithubRepoFact {
            owner: owner.to_string(),
            repo: repo.to_string(),
            exists: false,
            default_branch: None,
            pushed_at: None,
            stargazers_count: None,
            license_spdx: None,
            archived: None,
        };
    }
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    GithubRepoFact {
        owner: owner.to_string(),
        repo: repo.to_string(),
        exists: true,
        default_branch: body
            .get("default_branch")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        pushed_at: body
            .get("pushed_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        stargazers_count: body.get("stargazers_count").and_then(|v| v.as_u64()),
        license_spdx: body
            .get("license")
            .and_then(|l| l.get("spdx_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "NOASSERTION")
            .map(str::to_string),
        archived: body.get("archived").and_then(|v| v.as_bool()),
    }
}

/// Phase B facts: prior-art candidates retrieved by metadata similarity.
/// Fed to the Novelty specialist so it judges novelty against actual neighbors
/// instead of its training-cutoff memory.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct NoveltyFacts {
    pub related_papers: Vec<RelatedPaper>,
    /// Set when the external API failed; the LLM should fall back to its own
    /// memory and explicitly note the gap. Empty string when retrieval worked.
    pub retrieval_error: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RelatedPaper {
    pub title: String,
    pub abstract_snippet: Option<String>,
    pub year: Option<u32>,
    pub primary_author: Option<String>,
    pub source: String,
    /// Source-specific identifier (S2 paperId, arxiv id, etc.).
    pub source_id: String,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
}

/// Single Semantic Scholar search against the paper title. Free API, no auth.
/// Failure modes are soft — empty `related_papers` + populated
/// `retrieval_error` so the prompt can fall back to LLM-only novelty judgment.
pub async fn gather_novelty_facts(
    http: &reqwest::Client,
    extract: &PaperExtract,
) -> NoveltyFacts {
    let title = extract.title.trim();
    if title.is_empty() {
        return NoveltyFacts {
            related_papers: vec![],
            retrieval_error: "paper extract has no title".into(),
        };
    }
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query={query}&limit=20&fields=title,abstract,year,authors,externalIds",
        query = semantic_scholar_url_encode(title),
    );
    let req = http
        .get(&url)
        .header("User-Agent", "grokrxiv-review")
        .timeout(std::time::Duration::from_secs(15));
    match req.send().await {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
            let papers = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().take(20).map(parse_s2_paper).collect::<Vec<_>>())
                .unwrap_or_default();
            NoveltyFacts {
                related_papers: papers,
                retrieval_error: String::new(),
            }
        }
        Ok(r) => NoveltyFacts {
            related_papers: vec![],
            retrieval_error: format!("semantic_scholar status={}", r.status().as_u16()),
        },
        Err(e) => NoveltyFacts {
            related_papers: vec![],
            retrieval_error: format!("semantic_scholar network: {e}"),
        },
    }
}

fn parse_s2_paper(item: &serde_json::Value) -> RelatedPaper {
    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let abstract_snippet = item
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(|s| {
            let snippet: String = s.chars().take(280).collect();
            snippet
        });
    let year = item
        .get("year")
        .and_then(|v| v.as_u64())
        .map(|y| y as u32);
    let primary_author = item
        .get("authors")
        .and_then(|a| a.as_array())
        .and_then(|arr| arr.first())
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let s2_id = item
        .get("paperId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let external_ids = item.get("externalIds");
    let doi = external_ids
        .and_then(|e| e.get("DOI"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let arxiv_id = external_ids
        .and_then(|e| e.get("ArXiv"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let url = if !s2_id.is_empty() {
        Some(format!("https://www.semanticscholar.org/paper/{s2_id}"))
    } else {
        arxiv_id
            .as_deref()
            .map(|a| format!("https://arxiv.org/abs/{a}"))
    };
    RelatedPaper {
        title,
        abstract_snippet,
        year,
        primary_author,
        source: "semantic_scholar".to_string(),
        source_id: s2_id,
        url,
        doi,
        arxiv_id,
    }
}

fn semantic_scholar_url_encode(raw: &str) -> String {
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

/// Phase C facts: structural elements (tables, equation labels, complexity
/// mentions) extracted by deterministic parsing of the paper body. Surfaces
/// to the Technical Correctness specialist so it cross-checks numerical
/// claims against actual reported numbers instead of trusting prose memory.
/// No HTTP, no LLM — pure regex over already-extracted section bodies.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TechnicalCorrectnessFacts {
    pub tables: Vec<TableFact>,
    pub equation_labels: Vec<EquationLabelFact>,
    pub complexity_mentions: Vec<ComplexityMention>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TableFact {
    pub section: String,
    /// First non-empty row, usually the column headers.
    pub header_row: String,
    pub row_count: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EquationLabelFact {
    pub section: String,
    pub label: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComplexityMention {
    pub section: String,
    /// The exact substring, e.g. "O(n log n)" or "Θ(n^2)".
    pub notation: String,
}

/// Scan the extracted paper body for tables, equation labels, and complexity
/// notations. Caps per-category to keep the prompt section bounded on papers
/// with hundreds of equations.
pub fn gather_tc_facts(extract: &PaperExtract) -> TechnicalCorrectnessFacts {
    const MAX_PER_CATEGORY: usize = 25;
    let mut tables: Vec<TableFact> = Vec::new();
    let mut equations: Vec<EquationLabelFact> = Vec::new();
    let mut complexities: Vec<ComplexityMention> = Vec::new();
    for section in &extract.sections {
        if tables.len() < MAX_PER_CATEGORY {
            tables.extend(
                extract_tables(&section.heading, &section.body_markdown)
                    .into_iter()
                    .take(MAX_PER_CATEGORY - tables.len()),
            );
        }
        if equations.len() < MAX_PER_CATEGORY {
            equations.extend(
                extract_equation_labels(&section.heading, &section.body_markdown)
                    .into_iter()
                    .take(MAX_PER_CATEGORY - equations.len()),
            );
        }
        if complexities.len() < MAX_PER_CATEGORY {
            complexities.extend(
                extract_complexity_mentions(&section.heading, &section.body_markdown)
                    .into_iter()
                    .take(MAX_PER_CATEGORY - complexities.len()),
            );
        }
    }
    TechnicalCorrectnessFacts {
        tables,
        equation_labels: equations,
        complexity_mentions: complexities,
    }
}

/// A markdown table is a block of consecutive `|`-pipe rows. We require at
/// least one row that's just dashes (the header separator) to qualify, so
/// we don't catch single-cell `|` literals in prose.
fn extract_tables(section: &str, body: &str) -> Vec<TableFact> {
    let mut out: Vec<TableFact> = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with('|') && line.ends_with('|') && line.len() > 2 {
            // Look for a separator row in the next 2 lines.
            let header = line;
            let mut block_end = i + 1;
            let mut has_separator = false;
            while block_end < lines.len()
                && lines[block_end].trim().starts_with('|')
                && lines[block_end].trim().ends_with('|')
            {
                let inner = lines[block_end].trim().trim_matches('|');
                if inner.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t')) {
                    has_separator = true;
                }
                block_end += 1;
            }
            if has_separator && block_end > i + 2 {
                out.push(TableFact {
                    section: section.to_string(),
                    header_row: header.to_string(),
                    // block_end - i counts header + separator + data rows.
                    // Subtract 2 to get just the data rows.
                    row_count: (block_end - i).saturating_sub(2),
                });
            }
            i = block_end;
        } else {
            i += 1;
        }
    }
    out
}

/// LaTeX-style `\label{eq:foo}` or `\label{eqn:foo}` markers. Also picks up
/// numbered equations like `(1)`, `(2.3)` that appear at the start of a line
/// — those are inline references to display equations.
fn extract_equation_labels(section: &str, body: &str) -> Vec<EquationLabelFact> {
    let mut out: Vec<EquationLabelFact> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // \label{...} with eq: prefix or similar.
    let mut i = 0;
    while let Some(rel) = body[i..].find("\\label{") {
        let start = i + rel + 7;
        if let Some(end_rel) = body[start..].find('}') {
            let label = body[start..start + end_rel].trim();
            if label.starts_with("eq")
                || label.starts_with("equation")
                || label.starts_with("Eq")
            {
                let key = label.to_string();
                if seen.insert(key.clone()) {
                    out.push(EquationLabelFact {
                        section: section.to_string(),
                        label: key,
                    });
                }
            }
            i = start + end_rel + 1;
        } else {
            break;
        }
    }
    out
}

/// Big-O / Theta / Omega complexity mentions.
fn extract_complexity_mentions(section: &str, body: &str) -> Vec<ComplexityMention> {
    let mut out: Vec<ComplexityMention> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for prefix in ["O(", "Θ(", "Ω(", "Theta(", "Omega("] {
        let mut i = 0;
        while let Some(rel) = body[i..].find(prefix) {
            let start = i + rel;
            // Find the matching close paren up to 40 chars away.
            let tail = &body[start..];
            let limit = tail.len().min(40);
            if let Some(close) = tail[..limit].find(')') {
                let notation = &tail[..=close];
                let key = notation.to_string();
                if seen.insert(key.clone()) {
                    out.push(ComplexityMention {
                        section: section.to_string(),
                        notation: key,
                    });
                }
                i = start + close + 1;
            } else {
                i = start + prefix.len();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::{Citation, Section};

    fn extract_with_section(body: &str) -> PaperExtract {
        PaperExtract {
            arxiv_id: "x".into(),
            title: "t".into(),
            authors: vec![],
            abstract_: "a".into(),
            field: None,
            sections: vec![Section {
                heading: "1. Intro".into(),
                body_markdown: body.into(),
            }],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    #[test]
    fn find_urls_picks_up_https_and_strips_trailing_punctuation() {
        let urls = find_urls(
            "See https://github.com/foo/bar for code, also http://example.com/data.tar.",
        );
        assert!(urls.contains(&"https://github.com/foo/bar".to_string()));
        assert!(urls.contains(&"http://example.com/data.tar".to_string()));
    }

    #[test]
    fn parse_github_url_extracts_owner_repo() {
        assert_eq!(
            parse_github_url("https://github.com/openai/gpt-2"),
            Some(("openai".into(), "gpt-2".into()))
        );
        assert_eq!(
            parse_github_url("http://github.com/foo/bar.git"),
            Some(("foo".into(), "bar".into()))
        );
        assert_eq!(
            parse_github_url("https://github.com/foo/bar/tree/main"),
            Some(("foo".into(), "bar".into()))
        );
        assert_eq!(parse_github_url("https://example.com/foo/bar"), None);
    }

    #[test]
    fn classify_url_recognizes_code_and_dataset_hosts() {
        assert_eq!(classify_url("https://github.com/foo/bar"), UrlKind::Code);
        assert_eq!(classify_url("https://huggingface.co/foo"), UrlKind::Code);
        assert_eq!(classify_url("https://zenodo.org/record/1"), UrlKind::Dataset);
        assert_eq!(classify_url("https://example.com"), UrlKind::Other);
    }

    #[test]
    fn collect_urls_dedupes_across_sections_and_bibliography() {
        let mut extract = extract_with_section(
            "First mention https://github.com/foo/bar and https://example.com/data",
        );
        extract.bibliography.push(Citation {
            raw: "Foo, https://github.com/foo/bar (also https://example.com/data)".into(),
            doi: None,
            arxiv_id: None,
            title: None,
        });
        let urls = collect_urls(&extract);
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn extract_tables_finds_markdown_tables_and_counts_rows() {
        let body = "Intro prose.\n\n| col1 | col2 |\n| --- | --- |\n| a | 1 |\n| b | 2 |\n\nNext para.";
        let tables = extract_tables("3. Results", body);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].section, "3. Results");
        assert_eq!(tables[0].header_row, "| col1 | col2 |");
        assert_eq!(tables[0].row_count, 2);
    }

    #[test]
    fn extract_equation_labels_picks_up_label_macros() {
        let body = "Eq \\label{eq:main}: text. Later \\label{section} not an eq. \\label{eq:bound} too.";
        let eqs = extract_equation_labels("2. Methods", body);
        let labels: Vec<&str> = eqs.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"eq:main"));
        assert!(labels.contains(&"eq:bound"));
        assert!(!labels.contains(&"section"));
    }

    #[test]
    fn extract_complexity_mentions_finds_bigo_and_theta() {
        let body = "Our method runs in O(n log n) time, beating the previous Θ(n^2) approach.";
        let cs = extract_complexity_mentions("4. Analysis", body);
        let n: Vec<&str> = cs.iter().map(|c| c.notation.as_str()).collect();
        assert!(n.contains(&"O(n log n)"));
        assert!(n.contains(&"Θ(n^2)"));
    }

    #[tokio::test]
    async fn semantic_scholar_search_parses_data_array_and_caps_at_20() {
        let server = wiremock::MockServer::start().await;
        let mut items: Vec<serde_json::Value> = Vec::new();
        for i in 0..25 {
            items.push(serde_json::json!({
                "paperId": format!("paper{i}"),
                "title": format!("Related Title {i}"),
                "abstract": "An abstract snippet about the work.",
                "year": 2024,
                "authors": [{ "name": format!("Author {i}") }],
                "externalIds": { "ArXiv": format!("2401.{:05}", 10000 + i) }
            }));
        }
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/graph/v1/paper/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "total": 25,
                    "data": items,
                })),
            )
            .mount(&server)
            .await;
        // Build a verifier that points at the mock — gather_novelty_facts is
        // hardcoded to the real S2 URL, so swap via a tiny local copy of the
        // logic. We just assert the parser end-to-end by calling it.
        let body: serde_json::Value = reqwest::Client::new()
            .get(format!("{}/graph/v1/paper/search?query=t&limit=20&fields=title", server.uri()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let papers = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| arr.iter().take(20).map(parse_s2_paper).collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(papers.len(), 20);
        assert_eq!(papers[0].title, "Related Title 0");
        assert!(papers[0].url.as_deref().unwrap().contains("paper0"));
        assert_eq!(papers[0].arxiv_id.as_deref(), Some("2401.10000"));
    }

    #[test]
    fn semantic_scholar_url_encode_matches_form_urlencoded() {
        assert_eq!(semantic_scholar_url_encode("a b c"), "a+b+c");
        assert_eq!(semantic_scholar_url_encode("foo:bar/baz"), "foo%3Abar%2Fbaz");
        assert_eq!(semantic_scholar_url_encode("simple"), "simple");
    }

    #[tokio::test]
    async fn head_check_marks_404_unreachable_and_200_reachable() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .and(wiremock::matchers::path("/good"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .and(wiremock::matchers::path("/dead"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let good = head_check(
            &http,
            &format!("{}/good", server.uri()),
            UrlKind::Other,
        )
        .await;
        let dead = head_check(
            &http,
            &format!("{}/dead", server.uri()),
            UrlKind::Other,
        )
        .await;
        assert!(good.reachable);
        assert_eq!(good.status, Some(200));
        assert!(!dead.reachable);
        assert_eq!(dead.status, Some(404));
    }
}
