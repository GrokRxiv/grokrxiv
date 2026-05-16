//! GitHub PR opener for the `GrokRxiv/grokrxiv-reviews` repository.
//!
//! Implementation uses the raw `repos/.../git/...` REST APIs via the
//! `octocrab` crate. The flow:
//!
//! 1. Resolve `main`'s tip SHA.
//! 2. Create one blob per file via `git/blobs`.
//! 3. Build a tree from the blobs via `git/trees`.
//! 4. Create a commit pointing at the new tree, parented on `main` tip.
//! 5. Create a branch ref `refs/heads/review/<arxiv_id>-<short-uuid>`.
//! 6. Open a PR with the supplied title + body.
//!
//! This is **called only after explicit human admin approval** — never from
//! the automatic review pipeline. The [`crate::AdminCaller`] capability token
//! parameter is required and is enforced by the compiler.

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AdminCaller;

/// Owner/repo pair used to construct API URLs.
pub struct GithubPublisher {
    /// Pre-configured octocrab client (carries auth).
    pub client: octocrab::Octocrab,
    /// Repo owner (e.g. `GrokRxiv`).
    pub owner: String,
    /// Repo name (e.g. `grokrxiv-reviews`).
    pub repo: String,
    /// Base branch we PR against (default `main`).
    pub base: String,
}

impl GithubPublisher {
    /// Construct a publisher with the default `main` base branch.
    pub fn new(
        client: octocrab::Octocrab,
        owner: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            client,
            owner: owner.into(),
            repo: repo.into(),
            base: "main".to_string(),
        }
    }

    /// Override the base branch (mostly for tests).
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// Open a moderation PR. **Only call this after explicit human admin
    /// approval**; the [`AdminCaller`] argument is a compile-time fence. We
    /// take it by reference so the caller cannot accidentally hand off
    /// ownership and so future callers can hold one token for several
    /// approvals in the same admin session.
    pub async fn open_review_pr(
        &self,
        _caller: &AdminCaller,
        params: OpenReviewPr,
    ) -> Result<String> {
        let short = Uuid::new_v4().simple().to_string()[..8].to_string();
        let branch = format!("review/{}-{}", params.arxiv_id, short);
        // Build the PR body and assert it contains the required disclaimer
        // and the review-id marker the orchestrator's webhook depends on.
        let body_md = build_pr_body(&params)
            .context("PR body must include the public disclaimer and review-id marker")?;

        // 1) Resolve base tip SHA.
        let base_ref: GitRef = self
            .api_get(&format!("git/ref/heads/{}", self.base))
            .await
            .context("get base ref")?;
        let base_sha = base_ref.object.sha;

        // 2) Create blobs.
        let mut blobs: Vec<TreeEntry> = Vec::with_capacity(params.files.len());
        for (path, bytes) in &params.files {
            let blob: BlobResponse = self
                .api_post(
                    "git/blobs",
                    &BlobRequest {
                        content: base64::engine::general_purpose::STANDARD.encode(bytes),
                        encoding: "base64",
                    },
                )
                .await
                .with_context(|| format!("create blob {path}"))?;
            blobs.push(TreeEntry {
                path: path.clone(),
                mode: "100644".into(),
                r#type: "blob".into(),
                sha: blob.sha,
            });
        }

        // 3) Create tree.
        let tree: TreeResponse = self
            .api_post(
                "git/trees",
                &TreeRequest {
                    base_tree: Some(base_sha.clone()),
                    tree: blobs,
                },
            )
            .await
            .context("create tree")?;

        // 4) Create commit.
        let commit: CommitResponse = self
            .api_post(
                "git/commits",
                &CommitRequest {
                    message: params.title.clone(),
                    tree: tree.sha,
                    parents: vec![base_sha],
                },
            )
            .await
            .context("create commit")?;

        // 5) Create branch ref.
        self.api_post::<RefRequest, GitRef>(
            "git/refs",
            &RefRequest {
                r#ref: format!("refs/heads/{branch}"),
                sha: commit.sha,
            },
        )
        .await
        .context("create branch ref")?;

        // 6) Open PR.
        let pr: PullRequestResponse = self
            .api_post(
                "pulls",
                &PullRequestRequest {
                    title: params.title,
                    head: branch,
                    base: self.base.clone(),
                    body: body_md,
                },
            )
            .await
            .context("create pull request")?;
        Ok(pr.html_url)
    }

    /// Close an existing pull request and post a single explanatory comment.
    /// Used by the supersede flow: when a paper is re-reviewed and a new PR
    /// opens, the prior PR is closed with a pointer to the new one.
    ///
    /// Errors are returned but the caller should treat them as non-fatal —
    /// see callers in `supervisor::run_publish` and `cli::approve_impl`.
    pub async fn close_pr_with_comment(
        &self,
        _caller: &AdminCaller,
        pr_number: u64,
        comment_md: &str,
    ) -> Result<()> {
        // 1) Close the PR. Updates use PATCH /repos/{owner}/{repo}/pulls/{N}
        //    with `state: "closed"`.
        let _patched: PullRequestResponse = self
            .api_patch(
                &format!("pulls/{pr_number}"),
                &PullRequestUpdate {
                    state: "closed".into(),
                },
            )
            .await
            .with_context(|| format!("close PR #{pr_number}"))?;

        // 2) Post the explanatory comment. PRs use the *issues* comments
        //    endpoint — see https://docs.github.com/en/rest/issues/comments.
        let _: IssueCommentResponse = self
            .api_post(
                &format!("issues/{pr_number}/comments"),
                &IssueCommentRequest {
                    body: comment_md.to_string(),
                },
            )
            .await
            .with_context(|| format!("comment on PR #{pr_number}"))?;

        Ok(())
    }

    async fn api_get<T: serde::de::DeserializeOwned>(&self, suffix: &str) -> Result<T> {
        let url = format!("/repos/{}/{}/{suffix}", self.owner, self.repo);
        self.client.get(&url, None::<&()>).await.map_err(Into::into)
    }

    async fn api_post<B: Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        suffix: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("/repos/{}/{}/{suffix}", self.owner, self.repo);
        self.client.post(&url, Some(body)).await.map_err(Into::into)
    }

    async fn api_patch<B: Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        suffix: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("/repos/{}/{}/{suffix}", self.owner, self.repo);
        self.client
            .patch(&url, Some(body))
            .await
            .map_err(Into::into)
    }
}

/// Parse a GitHub pull-request URL like `https://github.com/<owner>/<repo>/pull/123`
/// and return the trailing `<N>` as a `u64`. Returns `None` if the URL doesn't
/// match the expected shape — callers should treat that as "nothing to close".
pub fn parse_pr_number(url: &str) -> Option<u64> {
    let (_, tail) = url.rsplit_once("/pull/")?;
    // Strip any trailing query / anchor / slash if present.
    let n_str = tail
        .split(|c: char| c == '/' || c == '?' || c == '#')
        .next()?;
    n_str.parse::<u64>().ok()
}

/// Inputs to [`GithubPublisher::open_review_pr`].
pub struct OpenReviewPr {
    /// arXiv id this review pertains to.
    pub arxiv_id: String,
    /// Primary arXiv field (used to populate `reviews/YYYY/MM/<field>/...`).
    pub field: String,
    /// Date of publication (calendar day in UTC).
    pub date: chrono::NaiveDate,
    /// Files to commit in `(path-in-repo, bytes)` form.
    pub files: Vec<(String, Vec<u8>)>,
    /// PR title — recommended format `Review: <paper title> (arXiv:<id>)`.
    pub title: String,
    /// `reviews.id` of the review this PR is for. The orchestrator's webhook
    /// handler greps the PR body for `grokrxiv-review-id: <uuid>` to correlate
    /// the merge back to the row — see `routes/webhook.rs::extract_review_id`.
    pub review_id: Uuid,
    /// Optional moderator-supplied markdown to inline in the PR body. The
    /// publisher always prepends the public disclaimer and appends the review
    /// id marker; this is intermediate prose only.
    pub body_md: String,
}

/// Build the canonical PR body. Enforces only the
/// `grokrxiv-review-id: <uuid>` marker the orchestrator's webhook handler
/// matches on. Disclaimers no longer appear here — the dedicated `/legal`
/// page on the web app is the single source of truth.
///
/// Returns an error if `params.body_md` itself contains the marker (to avoid
/// caller-supplied UUID smuggling).
fn build_pr_body(params: &OpenReviewPr) -> Result<String> {
    if params.body_md.contains("grokrxiv-review-id:") {
        anyhow::bail!("caller body_md may not contain the grokrxiv-review-id marker");
    }
    let mut out = String::new();
    out.push_str(params.body_md.trim());
    if !params.body_md.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!("grokrxiv-review-id: {}\n", params.review_id));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Wire types for the GitHub REST API.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GitRef {
    object: GitObject,
}

#[derive(Deserialize)]
struct GitObject {
    sha: String,
}

#[derive(Serialize)]
struct BlobRequest {
    content: String,
    encoding: &'static str,
}

#[derive(Deserialize)]
struct BlobResponse {
    sha: String,
}

#[derive(Serialize)]
struct TreeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    base_tree: Option<String>,
    tree: Vec<TreeEntry>,
}

#[derive(Serialize)]
struct TreeEntry {
    path: String,
    mode: String,
    r#type: String,
    sha: String,
}

#[derive(Deserialize)]
struct TreeResponse {
    sha: String,
}

#[derive(Serialize)]
struct CommitRequest {
    message: String,
    tree: String,
    parents: Vec<String>,
}

#[derive(Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Serialize)]
struct RefRequest {
    r#ref: String,
    sha: String,
}

#[derive(Serialize)]
struct PullRequestRequest {
    title: String,
    head: String,
    base: String,
    body: String,
}

#[derive(Deserialize)]
struct PullRequestResponse {
    #[serde(default)]
    html_url: String,
}

#[derive(Serialize)]
struct PullRequestUpdate {
    state: String,
}

#[derive(Serialize)]
struct IssueCommentRequest {
    body: String,
}

#[derive(Deserialize)]
struct IssueCommentResponse {
    #[allow(dead_code)]
    #[serde(default)]
    id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> octocrab::Octocrab {
        octocrab::OctocrabBuilder::new()
            .base_uri(server.uri())
            .expect("uri")
            .personal_token("FAKE".to_string())
            .build()
            .expect("octocrab build")
    }

    #[tokio::test]
    async fn open_review_pr_makes_expected_calls() {
        let server = MockServer::start().await;

        // GET base ref.
        Mock::given(method("GET"))
            .and(path_regex(
                r"^/repos/GrokRxiv/grokrxiv-reviews/git/ref/heads/main$",
            ))
            .and(header("authorization", "Bearer FAKE"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"object":{"sha":"abc123"}}"#),
            )
            .mount(&server)
            .await;
        // Create blob (one per file).
        Mock::given(method("POST"))
            .and(path_regex(r"^/repos/GrokRxiv/grokrxiv-reviews/git/blobs$"))
            .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"sha":"blobsha"}"#))
            .mount(&server)
            .await;
        // Create tree.
        Mock::given(method("POST"))
            .and(path_regex(r"^/repos/GrokRxiv/grokrxiv-reviews/git/trees$"))
            .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"sha":"treesha"}"#))
            .mount(&server)
            .await;
        // Create commit.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/repos/GrokRxiv/grokrxiv-reviews/git/commits$",
            ))
            .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"sha":"commitsha"}"#))
            .mount(&server)
            .await;
        // Create branch ref.
        Mock::given(method("POST"))
            .and(path_regex(r"^/repos/GrokRxiv/grokrxiv-reviews/git/refs$"))
            .respond_with(
                ResponseTemplate::new(201).set_body_string(r#"{"object":{"sha":"commitsha"}}"#),
            )
            .mount(&server)
            .await;
        // Open PR.
        Mock::given(method("POST"))
            .and(path_regex(r"^/repos/GrokRxiv/grokrxiv-reviews/pulls$"))
            .respond_with(ResponseTemplate::new(201).set_body_string(
                r#"{"html_url":"https://github.com/GrokRxiv/grokrxiv-reviews/pull/1"}"#,
            ))
            .mount(&server)
            .await;

        let publisher = GithubPublisher::new(client(&server), "GrokRxiv", "grokrxiv-reviews");
        let admin = AdminCaller::from_admin_endpoint();
        let url = publisher
            .open_review_pr(
                &admin,
                OpenReviewPr {
                    arxiv_id: "2401.12345".into(),
                    field: "hep-th".into(),
                    date: chrono::NaiveDate::from_ymd_opt(2026, 5, 13).unwrap(),
                    files: vec![
                        (
                            "reviews/2026/05/hep-th/2401.12345/review.html".into(),
                            b"<html><h1>x</h1></html>".to_vec(),
                        ),
                        (
                            "reviews/2026/05/hep-th/2401.12345/metadata.json".into(),
                            b"{}".to_vec(),
                        ),
                    ],
                    title: "Review: Modular Composition (arXiv:2401.12345)".into(),
                    review_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
                    body_md: "Moderator-approved review.".into(),
                },
            )
            .await
            .expect("open pr");
        assert_eq!(url, "https://github.com/GrokRxiv/grokrxiv-reviews/pull/1");
    }

    #[test]
    fn build_pr_body_prepends_disclaimer_and_appends_marker() {
        let id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let body = build_pr_body(&OpenReviewPr {
            arxiv_id: "2605.12484".into(),
            field: "cs".into(),
            date: chrono::NaiveDate::from_ymd_opt(2026, 5, 13).unwrap(),
            files: vec![],
            title: "x".into(),
            review_id: id,
            body_md: "Looks fine.".into(),
        })
        .expect("build");
        assert!(body.starts_with("Looks fine."));
        assert!(body.ends_with("grokrxiv-review-id: 33333333-3333-3333-3333-333333333333\n"));
    }

    #[tokio::test]
    async fn close_pr_with_comment_fires_patch_and_comment() {
        let server = MockServer::start().await;

        let patch_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let comment_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let patch_hits_c = patch_hits.clone();
        let comment_hits_c = comment_hits.clone();

        Mock::given(method("PATCH"))
            .and(path_regex(r"^/repos/GrokRxiv/grokrxiv-reviews/pulls/17$"))
            .and(header("authorization", "Bearer FAKE"))
            .respond_with(move |_: &wiremock::Request| {
                patch_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(
                    r#"{"html_url":"https://github.com/GrokRxiv/grokrxiv-reviews/pull/17","state":"closed"}"#,
                )
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path_regex(
                r"^/repos/GrokRxiv/grokrxiv-reviews/issues/17/comments$",
            ))
            .and(header("authorization", "Bearer FAKE"))
            .respond_with(move |_: &wiremock::Request| {
                comment_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(201).set_body_string(r#"{"id":1234}"#)
            })
            .mount(&server)
            .await;

        let publisher = GithubPublisher::new(client(&server), "GrokRxiv", "grokrxiv-reviews");
        let admin = AdminCaller::from_admin_endpoint();
        publisher
            .close_pr_with_comment(
                &admin,
                17,
                "Superseded by #42.\nThe new review run incorporated extraction-pipeline fixes.",
            )
            .await
            .expect("close should succeed");

        assert_eq!(
            patch_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "PATCH /pulls/17 must fire exactly once",
        );
        assert_eq!(
            comment_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "POST /issues/17/comments must fire exactly once",
        );
    }

    #[test]
    fn parse_pr_number_extracts_trailing_id() {
        assert_eq!(
            parse_pr_number("https://github.com/GrokRxiv/grokrxiv-reviews/pull/17"),
            Some(17),
        );
        assert_eq!(
            parse_pr_number("https://github.com/GrokRxiv/grokrxiv-reviews/pull/123/files"),
            Some(123),
        );
        assert_eq!(
            parse_pr_number("https://github.com/GrokRxiv/grokrxiv-reviews/pull/9?w=1"),
            Some(9),
        );
        assert_eq!(parse_pr_number("https://example.com/no-pull-segment"), None);
        assert_eq!(
            parse_pr_number("https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-abc123"),
            None,
        );
    }

    #[test]
    fn build_pr_body_rejects_caller_injected_marker() {
        let err = build_pr_body(&OpenReviewPr {
            arxiv_id: "2605.12484".into(),
            field: "cs".into(),
            date: chrono::NaiveDate::from_ymd_opt(2026, 5, 13).unwrap(),
            files: vec![],
            title: "x".into(),
            review_id: Uuid::nil(),
            body_md: "evil grokrxiv-review-id: 00000000-0000-0000-0000-000000000000".into(),
        })
        .expect_err("should reject");
        assert!(err.to_string().contains("grokrxiv-review-id"));
    }
}
