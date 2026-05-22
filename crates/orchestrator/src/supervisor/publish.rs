use std::time::Duration;

use super::WorkItem;
use crate::state::AppState;
use uuid::Uuid;

#[cfg(feature = "grokrxiv-publisher")]
/// Spawn a background reconciler that repairs `pr_open` reviews whose GitHub PR
/// was merged but whose webhook delivery was missed.
pub fn spawn_publish_reconcile(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = reconcile_published_reviews_once(&state).await {
                tracing::warn!(err = %format!("{e:#}"), "publish reconcile failed");
            }
        }
    })
}

#[cfg(feature = "grokrxiv-publisher")]
async fn reconcile_published_reviews_once(state: &AppState) -> anyhow::Result<()> {
    let Some(pool) = state.db.as_ref() else {
        return Ok(());
    };
    let token = match std::env::var("GITHUB_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            tracing::warn!("publish reconcile skipped: GITHUB_TOKEN not set");
            return Ok(());
        }
    };
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let reviews = crate::db::list_pr_open_reviews_with_urls(pool, 100).await?;
    let lookup = GithubPublishPrLookup { client };
    let finalizer = StatePublishFinalizer { state };
    let stats = reconcile_published_reviews_with(reviews, &lookup, &finalizer).await;
    tracing::info!(
        checked = stats.checked,
        finalized = stats.finalized,
        skipped_malformed = stats.skipped_malformed,
        lookup_errors = stats.lookup_errors,
        finalize_errors = stats.finalize_errors,
        "publish reconcile pass complete"
    );
    Ok(())
}

#[cfg(feature = "grokrxiv-publisher")]
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PublishReconcileStats {
    pub(super) checked: usize,
    pub(super) finalized: usize,
    pub(super) skipped_malformed: usize,
    pub(super) lookup_errors: usize,
    pub(super) finalize_errors: usize,
}

#[cfg(feature = "grokrxiv-publisher")]
#[async_trait::async_trait]
pub(super) trait PublishPrLookup: Send + Sync {
    async fn is_pr_merged(&self, owner: &str, repo: &str, number: u64) -> anyhow::Result<bool>;
}

#[cfg(feature = "grokrxiv-publisher")]
#[async_trait::async_trait]
pub(super) trait PublishFinalizer: Send + Sync {
    async fn finalize(&self, review_id: Uuid) -> anyhow::Result<bool>;
}

#[cfg(feature = "grokrxiv-publisher")]
struct GithubPublishPrLookup {
    client: octocrab::Octocrab,
}

#[cfg(feature = "grokrxiv-publisher")]
#[async_trait::async_trait]
impl PublishPrLookup for GithubPublishPrLookup {
    async fn is_pr_merged(&self, owner: &str, repo: &str, number: u64) -> anyhow::Result<bool> {
        let pr = self.client.pulls(owner, repo).get(number).await?;
        Ok(pr.merged_at.is_some())
    }
}

#[cfg(feature = "grokrxiv-publisher")]
struct StatePublishFinalizer<'a> {
    state: &'a AppState,
}

#[cfg(feature = "grokrxiv-publisher")]
#[async_trait::async_trait]
impl PublishFinalizer for StatePublishFinalizer<'_> {
    async fn finalize(&self, review_id: Uuid) -> anyhow::Result<bool> {
        crate::routes::webhook::finalize_published_review(self.state, review_id).await
    }
}

#[cfg(feature = "grokrxiv-publisher")]
pub(super) async fn reconcile_published_reviews_with<L, F>(
    reviews: Vec<(Uuid, String)>,
    lookup: &L,
    finalizer: &F,
) -> PublishReconcileStats
where
    L: PublishPrLookup,
    F: PublishFinalizer,
{
    let mut stats = PublishReconcileStats::default();
    for (review_id, pr_url) in reviews {
        let Some((owner, repo, number)) = parse_github_pr_url(&pr_url) else {
            stats.skipped_malformed += 1;
            tracing::warn!(%review_id, %pr_url, "publish reconcile skipped malformed PR URL");
            continue;
        };
        stats.checked += 1;
        match lookup.is_pr_merged(&owner, &repo, number).await {
            Ok(true) => match finalizer.finalize(review_id).await {
                Ok(true) => stats.finalized += 1,
                Ok(false) => {}
                Err(e) => {
                    stats.finalize_errors += 1;
                    tracing::warn!(%review_id, %pr_url, err = %e, "publish reconcile finalizer failed");
                }
            },
            Ok(false) => {}
            Err(e) => {
                stats.lookup_errors += 1;
                tracing::warn!(%review_id, %pr_url, err = %e, "publish reconcile could not read PR");
            }
        }
    }
    stats
}

#[cfg(feature = "grokrxiv-publisher")]
pub(super) fn parse_github_pr_url(url: &str) -> Option<(String, String, u64)> {
    let path = url.strip_prefix("https://github.com/")?;
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pull" {
        return None;
    }
    let number = parts
        .next()?
        .split(|c| matches!(c, '?' | '#' | '/'))
        .next()?
        .parse()
        .ok()?;
    Some((owner, repo, number))
}

#[cfg(feature = "grokrxiv-ingest")]
/// Persist moderator-accepted revision selections and return the draft PR URL.
pub async fn apply_revisions(
    state: &AppState,
    review_id: Uuid,
    accepted_indices: Vec<i32>,
) -> anyhow::Result<String> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("apply_revisions: DATABASE_URL not configured"))?;

    // Gate revision application on the current review lifecycle state.
    let (status, mode) = crate::db::get_review_status_and_mode(pool, review_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("apply_revisions: review {review_id} not found"))?;
    if status != "awaiting_moderation" && status != "pr_open" {
        anyhow::bail!(
            "apply_revisions: review {review_id} is in status `{status}`; \
             expected `awaiting_moderation` or `pr_open`"
        );
    }
    if mode != "review_and_revise" {
        anyhow::bail!(
            "apply_revisions: review {review_id} ran in mode `{mode}`; \
             revision patches are only produced under `review_and_revise`"
        );
    }

    let rows = crate::db::list_revision_patches(pool, review_id).await?;
    if rows.is_empty() {
        anyhow::bail!(
            "apply_revisions: review {review_id} has no revision_patches rows; \
             nothing to apply"
        );
    }

    // The current revision path records the accepted patches and returns a
    // simulated draft PR URL until the LaTeX patching path owns real PR creation.
    let simulated_pr = format!(
        "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-revisions-{}",
        &review_id.simple().to_string()[..8]
    );

    // Partition global accepted indices into each row's local patch index space.
    let mut offset: i32 = 0;
    let accepted_set: std::collections::HashSet<i32> = accepted_indices.iter().copied().collect();
    for row in &rows {
        let patch_count = row.patches.as_array().map(|a| a.len() as i32).unwrap_or(0);
        let mut row_accepted: Vec<i32> = Vec::new();
        for local in 0..patch_count {
            let global = offset + local;
            if accepted_set.contains(&global) {
                row_accepted.push(local);
            }
        }
        offset += patch_count;
        crate::db::update_revision_patches_accepted(
            pool,
            row.id,
            &row_accepted,
            Some(&simulated_pr),
        )
        .await?;
    }

    tracing::info!(
        %review_id,
        pr_url = %simulated_pr,
        rows = rows.len(),
        accepted = accepted_indices.len(),
        "apply_revisions: stub PR materialised; DB updated"
    );
    Ok(simulated_pr)
}

#[cfg(feature = "grokrxiv-publisher")]
pub(super) async fn run_publish(state: &AppState, item: &WorkItem) -> anyhow::Result<()> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let review_id = item
        .ref_id
        .ok_or_else(|| anyhow::anyhow!("run_publish: ref_id (review id) required"))?;

    let row = crate::db::load_publish_review(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let crate::db::PublishReviewRow {
        review_id: _review_row_id,
        status,
        github_pr_url,
        arxiv_id,
        title,
        field,
        paper_id,
        visibility,
        source_kind,
        source_id,
    } = row;
    if let Some(existing) = real_pr_url(github_pr_url.as_deref()) {
        tracing::info!(
            %review_id,
            status = %status,
            pr_url = %existing,
            "publish idempotency: review already has a real PR URL"
        );
        return Ok(());
    }
    let source_ref =
        crate::source_display::source_display_ref(&source_kind, source_id.as_deref(), &arxiv_id);
    let artifact_id = crate::source_display::source_artifact_id(source_id.as_deref(), &arxiv_id);

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{artifact_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        artifact_id = artifact_id,
    );
    for name in ["review.html", "review.md", "review.tex", "bundle.zip"] {
        let path = dir_local.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            files.push((format!("{repo_prefix}/{name}"), bytes));
        }
    }
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts under artifacts/{review_id} — \
             re-run `agenthero grokrxiv ingest <arxiv_id>` to regenerate."
        );
    }

    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; required to open review PR"))?;

    let (owner, repo) = review_repo_for_visibility(&visibility);
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner, repo);
    let admin = AdminCaller::from_admin_endpoint();
    let pr_title = format!("Review: {} ({})", title, source_ref);
    let body_md = if visibility == "private" {
        "Approved by supervisor `run_publish`. \
             Private review: dashboard-only unless archived in the private reviews repo. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
            .to_string()
    } else {
        "Approved by supervisor `run_publish`. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
            .to_string()
    };
    let params = OpenReviewPr {
        arxiv_id: artifact_id,
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: pr_title,
        review_id,
        body_md,
        correction_source_path: None,
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;
    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = crate::db::set_review_github_pr_url(pool, review_id, &pr_url).await;
    tracing::info!(%review_id, %pr_url, "publish complete");

    close_superseded_pr_if_any(pool, &publisher, &admin, paper_id, &pr_url).await;
    Ok(())
}

#[cfg(feature = "grokrxiv-publisher")]
pub(super) fn real_pr_url(url: Option<&str>) -> Option<&str> {
    url.filter(|value| !value.contains("SIMULATED-") && parse_github_pr_url(value).is_some())
}

#[cfg(feature = "grokrxiv-publisher")]
fn review_repo_for_visibility(visibility: &str) -> (String, String) {
    match visibility {
        "private" => repo_from_combined_env(
            "GROKRXIV_PRIVATE_REVIEWS_REPO",
            "GrokRxiv",
            "grokrxiv-private-reviews",
        ),
        _ => {
            if let Some(repo) = repo_from_combined_env_optional("GROKRXIV_PUBLIC_REVIEWS_REPO") {
                repo
            } else {
                repo_from_legacy_public_env()
            }
        }
    }
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_legacy_public_env() -> (String, String) {
    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo_raw =
        std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "grokrxiv-reviews".into());
    split_owner_repo(&repo_raw).unwrap_or((owner, repo_raw))
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_combined_env(var: &str, default_owner: &str, default_repo: &str) -> (String, String) {
    repo_from_combined_env_optional(var)
        .unwrap_or_else(|| (default_owner.to_string(), default_repo.to_string()))
}

#[cfg(feature = "grokrxiv-publisher")]
fn repo_from_combined_env_optional(var: &str) -> Option<(String, String)> {
    let raw = std::env::var(var).ok()?;
    split_owner_repo(&raw)
}

#[cfg(feature = "grokrxiv-publisher")]
fn split_owner_repo(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let (owner, repo) = trimmed.split_once('/')?;
    let owner = owner.trim();
    let repo = repo.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Look up the most recently superseded review's PR URL for this paper and,
/// if found, close that PR on the moderation repo with a comment pointing at
/// the new one. Failures here are logged but never fail the new-PR open path
/// — the PR may already have been closed by hand, the GitHub token may have
/// lost scope, etc.
#[cfg(feature = "grokrxiv-publisher")]
async fn close_superseded_pr_if_any(
    pool: &sqlx::PgPool,
    publisher: &grokrxiv_publisher::GithubPublisher,
    admin: &grokrxiv_publisher::AdminCaller,
    paper_id: Uuid,
    new_pr_url: &str,
) {
    let prior = match crate::db::fetch_superseded_pr_url(pool, paper_id).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::warn!(%paper_id, err = %e, "supersede: fetch_superseded_pr_url failed");
            return;
        }
    };
    let Some(prior_url) = prior else { return };
    let Some(prior_n) = grokrxiv_publisher::parse_pr_number(&prior_url) else {
        tracing::warn!(
            %paper_id,
            %prior_url,
            "supersede: prior PR URL did not parse to a numeric id (simulated PR?)",
        );
        return;
    };
    let new_n_str = grokrxiv_publisher::parse_pr_number(new_pr_url)
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| new_pr_url.to_string());
    let comment = format!(
        "Superseded by {new_n_str}.\n\
         The new review run incorporated extraction-pipeline fixes and the prior review row was transitioned to status='withdrawn'.",
    );
    if let Err(e) = publisher
        .close_pr_with_comment(admin, prior_n, &comment)
        .await
    {
        tracing::warn!(
            %paper_id,
            prior_pr = %prior_url,
            err = %e,
            "supersede: close_pr_with_comment failed — leaving prior PR as-is (likely already closed)",
        );
    } else {
        tracing::info!(
            %paper_id,
            prior_pr = %prior_url,
            new_pr = %new_pr_url,
            "supersede: closed prior PR",
        );
    }
}

// Stub variants used when the matching feature isn't active so the supervisor
// still compiles in the minimal `--no-default-features` build.

#[cfg(not(feature = "grokrxiv-publisher"))]
pub(super) async fn run_publish(_state: &AppState, _item: &WorkItem) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "run_publish requires --features full (grokrxiv-publisher)"
    ))
}
