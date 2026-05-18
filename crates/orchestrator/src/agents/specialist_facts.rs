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
