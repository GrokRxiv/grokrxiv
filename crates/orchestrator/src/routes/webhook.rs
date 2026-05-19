//! `POST /webhook/github` — GitHub PR lifecycle webhook.
//!
//! Hardening:
//!
//! * Verifies the `X-Hub-Signature-256` HMAC against
//!   `GITHUB_WEBHOOK_SECRET`.
//! * Publishes on `pull_request.closed` events with `merged = true`.
//! * Re-runs review on `pull_request.synchronize` correction commits.
//! * Verifies review branches match the publisher's `review/<source>-<short>`
//!   pattern. Anything else is ignored as 200 OK so GitHub stops redelivering.
//! * On a verified merge: updates the review row to `published` and posts to
//!   `WEB_REVALIDATE_URL` with `REVALIDATE_SECRET`.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

/// Handle a GitHub webhook.
pub async fn github(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(secret) = state.config.github_webhook_secret.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "GITHUB_WEBHOOK_SECRET not configured" })),
        )
            .into_response();
    };
    let Some(sig_header) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing X-Hub-Signature-256" })),
        )
            .into_response();
    };
    if !verify_signature(secret.as_bytes(), &body, sig_header) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "bad signature" })),
        )
            .into_response();
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("bad json: {e}") })),
            )
                .into_response();
        }
    };

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !event.is_empty() && event != "pull_request" {
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "event": event })),
        )
            .into_response();
    }
    let delivery_id = headers
        .get("X-GitHub-Delivery")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if action == "synchronize" {
        return handle_pull_request_synchronize(state, &payload, delivery_id.as_deref())
            .await
            .into_response();
    }

    let merged = payload
        .get("pull_request")
        .and_then(|pr| pr.get("merged"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if action != "closed" || !merged {
        // Not a merge event; ack and ignore.
        return (StatusCode::OK, Json(json!({ "ignored": true }))).into_response();
    }

    let branch = payload
        .get("pull_request")
        .and_then(|pr| pr.get("head"))
        .and_then(|h| h.get("ref"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_valid_review_branch(branch) {
        // Not one of ours; ack and ignore so GitHub stops re-delivering.
        return (StatusCode::OK, Json(json!({ "ignored": true }))).into_response();
    }

    // Correlate to a review_id from the PR body marker.
    let review_id_opt = payload
        .get("pull_request")
        .and_then(|pr| pr.get("body"))
        .and_then(Value::as_str)
        .and_then(extract_review_id_from_body);

    let Some(review_id) = review_id_opt else {
        tracing::warn!("merge webhook: pr body missing grokrxiv-review-id marker");
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "branch": branch })),
        )
            .into_response();
    };

    // Gate the revalidate call on actually flipping a DB row to `published`.
    let mut updated = false;
    if let Some(pool) = state.db.as_ref() {
        match crate::db::set_review_status(
            pool,
            review_id,
            grokrxiv_schemas::ReviewStatus::Published,
            Some(chrono::Utc::now()),
        )
        .await
        {
            Ok(rows) if rows > 0 => updated = true,
            Ok(_) => tracing::warn!(review_id = %review_id, "merge webhook: no review row updated"),
            Err(e) => {
                tracing::error!(err = %e, review_id = %review_id, "merge webhook: db update failed")
            }
        }
    }

    if !updated {
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "branch": branch })),
        )
            .into_response();
    }

    spawn_revalidate(&state, review_id);

    (
        StatusCode::OK,
        Json(json!({ "ok": true, "review_id": review_id, "branch": branch })),
    )
        .into_response()
}

async fn handle_pull_request_synchronize(
    state: AppState,
    payload: &Value,
    delivery_id: Option<&str>,
) -> (StatusCode, Json<Value>) {
    let Some(pool) = state.db.as_ref() else {
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "reason": "DATABASE_URL not configured" })),
        );
    };
    let pr_body = payload
        .get("pull_request")
        .and_then(|pr| pr.get("body"))
        .and_then(Value::as_str);
    let review_id = pr_body.and_then(extract_review_id_from_body);
    let correction_source_path = pr_body.and_then(extract_correction_source_path_from_body);
    let Some(prior_review_id) = review_id else {
        tracing::warn!("synchronize webhook: pr body missing grokrxiv-review-id marker");
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "reason": "missing review marker" })),
        );
    };

    let branch = payload
        .get("pull_request")
        .and_then(|pr| pr.get("head"))
        .and_then(|h| h.get("ref"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_valid_review_branch(branch) {
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "branch": branch })),
        );
    }

    let paper_id: Uuid = match sqlx::query_scalar("select paper_id from reviews where id = $1")
        .bind(prior_review_id)
        .fetch_one(pool)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(%prior_review_id, err = %e, "synchronize webhook: review row not found");
            return (
                StatusCode::OK,
                Json(json!({ "ignored": true, "reason": "review not found" })),
            );
        }
    };
    let pr_number = payload
        .get("pull_request")
        .and_then(|pr| pr.get("number"))
        .or_else(|| payload.get("number"))
        .and_then(Value::as_i64);
    let pr_url = payload
        .get("pull_request")
        .and_then(|pr| pr.get("html_url"))
        .and_then(Value::as_str);
    let head_sha = payload
        .get("pull_request")
        .and_then(|pr| pr.get("head"))
        .and_then(|h| h.get("sha"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if head_sha.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({ "ignored": true, "reason": "missing head sha" })),
        );
    }
    let repo_owner = payload
        .get("repository")
        .and_then(|r| r.get("owner"))
        .and_then(|o| o.get("login"))
        .and_then(Value::as_str)
        .unwrap_or("GrokRxiv");
    let head_repo_clone_url = payload
        .get("pull_request")
        .and_then(|pr| pr.get("head"))
        .and_then(|h| h.get("repo"))
        .and_then(|r| r.get("clone_url"))
        .and_then(Value::as_str);
    let repo_name = payload
        .get("repository")
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("grokrxiv-reviews");
    let sender = payload
        .get("sender")
        .and_then(|s| s.get("login"))
        .and_then(Value::as_str);

    let event_payload = json!({
        "action": "synchronize",
        "pr_number": pr_number,
        "pr_url": pr_url,
        "head_ref": branch,
        "head_sha": head_sha,
        "head_repo_clone_url": head_repo_clone_url,
        "correction_source_path": correction_source_path,
        "repo_owner": repo_owner,
        "repo_name": repo_name,
        "sender": sender,
    });
    match crate::db::insert_review_event(
        pool,
        Some(prior_review_id),
        Some(paper_id),
        "github_pr_synchronize",
        "github",
        &event_payload,
        delivery_id,
    )
    .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::OK,
                Json(json!({ "ignored": true, "reason": "duplicate delivery" })),
            );
        }
        Err(e) => {
            tracing::warn!(%prior_review_id, err = %e, "synchronize webhook: event insert failed");
        }
    }

    let _ = crate::db::upsert_github_review_thread(
        pool,
        prior_review_id,
        paper_id,
        repo_owner,
        repo_name,
        pr_number,
        pr_url,
        Some(branch),
        Some(head_sha),
    )
    .await;

    let request_id = match crate::db::enqueue_rereview_for_commit(
        pool,
        paper_id,
        prior_review_id,
        head_sha,
        sender,
        Some("GitHub PR synchronize: author correction commit"),
    )
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => {
            return (
                StatusCode::OK,
                Json(
                    json!({ "ignored": true, "reason": "duplicate commit", "head_sha": head_sha }),
                ),
            );
        }
        Err(e) => {
            tracing::error!(%prior_review_id, %paper_id, err = %e, "synchronize webhook: enqueue re-review failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "enqueue re-review failed" })),
            );
        }
    };

    spawn_rereview_for_correction(
        state.clone(),
        request_id,
        paper_id,
        prior_review_id,
        repo_owner.to_string(),
        repo_name.to_string(),
        pr_number,
        pr_url.map(str::to_owned),
        branch.to_string(),
        head_sha.to_string(),
        head_repo_clone_url.map(str::to_owned),
        correction_source_path.map(str::to_owned),
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "rereview_request_id": request_id,
            "paper_id": paper_id,
            "prior_review_id": prior_review_id,
            "head_sha": head_sha,
        })),
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_rereview_for_correction(
    state: AppState,
    request_id: Uuid,
    paper_id: Uuid,
    prior_review_id: Uuid,
    repo_owner: String,
    repo_name: String,
    pr_number: Option<i64>,
    pr_url: Option<String>,
    head_ref: String,
    head_sha: String,
    head_repo_clone_url: Option<String>,
    correction_source_path: Option<String>,
) {
    #[cfg(feature = "grokrxiv-ingest")]
    tokio::spawn(async move {
        let Some(pool) = state.db.as_ref() else {
            return;
        };
        let _ = crate::db::mark_rereview_running(pool, request_id).await;
        match run_correction_review(
            &state,
            paper_id,
            &head_sha,
            head_repo_clone_url.as_deref(),
            correction_source_path.as_deref(),
        )
        .await
        {
            Ok(new_review_id) => {
                let _ = crate::db::mark_rereview_done(pool, request_id, new_review_id).await;
                let _ = crate::db::insert_review_event(
                    pool,
                    Some(new_review_id),
                    Some(paper_id),
                    "rereview_completed",
                    "github",
                    &json!({
                        "prior_review_id": prior_review_id,
                        "request_id": request_id,
                        "head_ref": head_ref,
                        "head_sha": head_sha,
                    }),
                    None,
                )
                .await;
                post_rereview_gate_feedback(
                    &state,
                    pool,
                    prior_review_id,
                    new_review_id,
                    paper_id,
                    &repo_owner,
                    &repo_name,
                    pr_number,
                    pr_url.as_deref(),
                    head_ref.as_str(),
                    head_sha.as_str(),
                )
                .await;
            }
            Err(e) => {
                let error = format!("{e:#}");
                let _ = crate::db::mark_rereview_failed(pool, request_id, &error).await;
                tracing::error!(
                    %request_id,
                    %paper_id,
                    %prior_review_id,
                    err = %error,
                    "GitHub correction re-review failed"
                );
            }
        }
    });

    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = (
            state,
            request_id,
            paper_id,
            prior_review_id,
            repo_owner,
            repo_name,
            pr_number,
            pr_url,
            head_ref,
            head_sha,
            head_repo_clone_url,
            correction_source_path,
        );
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn run_correction_review(
    state: &AppState,
    paper_id: Uuid,
    head_sha: &str,
    head_repo_clone_url: Option<&str>,
    correction_source_path: Option<&str>,
) -> anyhow::Result<Uuid> {
    if let Some((extract, source)) = prepare_git_correction_extract(
        state,
        paper_id,
        head_sha,
        head_repo_clone_url,
        correction_source_path,
    )
    .await?
    {
        if let Some(pool) = state.db.as_ref() {
            let _ =
                crate::db::update_paper_source_snapshot(pool, paper_id, &extract, &source).await;
        }
        return crate::supervisor::run_review_for_extract_blocking(state, paper_id, extract).await;
    }
    crate::supervisor::run_review_for_paper_blocking(state, paper_id).await
}

#[cfg(feature = "grokrxiv-ingest")]
async fn prepare_git_correction_extract(
    state: &AppState,
    paper_id: Uuid,
    head_sha: &str,
    head_repo_clone_url: Option<&str>,
    correction_source_path: Option<&str>,
) -> anyhow::Result<
    Option<(
        grokrxiv_schemas::PaperExtract,
        crate::db::PaperSourceMetadata,
    )>,
> {
    let Some(pool) = state.db.as_ref() else {
        return Ok(None);
    };
    let row: Option<(
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        serde_json::Value,
    )> = sqlx::query_as(
        "select source_kind, source_id, source_uri, title, field, source_metadata \
         from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    let Some((source_kind, stable_source_id, _source_uri, title, field, source_metadata)) = row
    else {
        return Ok(None);
    };
    let stable_source_id = stable_source_id.unwrap_or_else(|| format!("git-repo-{paper_id}"));
    let adapter = source_metadata
        .get("adapter")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let (repo, rev, paper_path) = if let Some(correction_path) = correction_source_path {
        let repo = head_repo_clone_url
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("correction PR is missing head repo clone URL"))?;
        (
            repo,
            Some(head_sha.to_string()),
            Some(std::path::PathBuf::from(correction_path)),
        )
    } else {
        if source_kind != "git_repo" {
            return Ok(None);
        }
        let repo = head_repo_clone_url
            .map(str::to_owned)
            .or_else(|| {
                adapter
                    .get("repo")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(|| anyhow::anyhow!("git correction source is missing repo URL"))?;
        let paper_path = adapter
            .get("paper_path")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from);
        (repo, Some(head_sha.to_string()), paper_path)
    };
    let prepared = grokrxiv_ingest::prepare_git_repo_source(
        &repo,
        rev.as_deref(),
        paper_path.as_deref(),
        Some(title),
        Vec::new(),
        field,
    )
    .await?;
    let display_label = prepared.identity.display_label.clone();
    let canonical_uri = prepared.identity.canonical_uri.clone();
    let arxiv_id = prepared.identity.arxiv_id.clone();
    let content_hash = prepared.identity.content_hash.clone();
    let source_metadata = serde_json::json!({
        "display_label": display_label,
        "canonical_uri": canonical_uri,
        "arxiv_id": arxiv_id,
        "adapter": prepared.source_metadata,
        "stable_source_id": stable_source_id.clone(),
        "correction_source_path": correction_source_path,
    });
    let source = crate::db::PaperSourceMetadata {
        source_kind,
        source_id: stable_source_id,
        source_uri: Some(canonical_uri),
        source_hash: Some(content_hash),
        source_metadata,
    };
    Ok(Some((prepared.extract, source)))
}

#[cfg(feature = "grokrxiv-ingest")]
#[allow(clippy::too_many_arguments)]
async fn post_rereview_gate_feedback(
    state: &AppState,
    pool: &sqlx::PgPool,
    prior_review_id: Uuid,
    new_review_id: Uuid,
    paper_id: Uuid,
    repo_owner: &str,
    repo_name: &str,
    pr_number: Option<i64>,
    pr_url: Option<&str>,
    head_ref: &str,
    head_sha: &str,
) {
    let Some(pr_number) = pr_number else {
        return;
    };
    let meta: Option<Value> = sqlx::query_scalar("select meta_review from reviews where id = $1")
        .bind(new_review_id)
        .fetch_one(pool)
        .await
        .unwrap_or(None);
    let recommendation = meta
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let marker_key = format!("review-{prior_review_id}");
    let specialist_gate = match crate::db::load_specialist_gate_for_review(pool, new_review_id)
        .await
    {
        Ok(gate) => gate,
        Err(e) => {
            tracing::warn!(%new_review_id, err = %e, "failed to load specialist gate for re-review feedback");
            crate::review_gate::SpecialistGate::evaluate(
                &[],
                crate::review_dag::DEFAULT_MIN_SPECIALIST_QUORUM,
                crate::review_dag::canonical_specialist_roles().len(),
            )
        }
    };
    let (body, failure) = rereview_gate_comment_body(
        new_review_id,
        Some(recommendation),
        meta.as_ref(),
        specialist_gate,
    );
    if let Some(failure) = failure.as_ref() {
        let _ = crate::github_feedback::record_gate_failure(state, new_review_id, failure).await;
    }
    match crate::github_feedback::post_or_update_gate_feedback_comment(
        state,
        repo_owner,
        repo_name,
        pr_number,
        &marker_key,
        &body,
    )
    .await
    {
        Ok(Some(comment)) => {
            if let Ok(comment_id) = i64::try_from(comment.comment_id) {
                let _ = crate::db::upsert_github_review_thread(
                    pool,
                    new_review_id,
                    paper_id,
                    repo_owner,
                    repo_name,
                    Some(pr_number),
                    pr_url,
                    Some(head_ref),
                    Some(head_sha),
                )
                .await;
                let _ = crate::db::update_github_feedback_comment(
                    pool,
                    new_review_id,
                    comment_id,
                    &comment.html_url,
                )
                .await;
                let _ = crate::db::attach_gate_feedback_comment(
                    pool,
                    new_review_id,
                    comment_id,
                    &comment.html_url,
                )
                .await;
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(%new_review_id, err = %e, "GitHub gate-feedback comment failed");
        }
    }
}

fn rereview_gate_comment_body(
    new_review_id: Uuid,
    recommendation: Option<&str>,
    meta: Option<&Value>,
    specialist_gate: crate::review_gate::SpecialistGate,
) -> (String, Option<crate::github_feedback::GateFailureArtifact>) {
    let gate =
        crate::review_gate::PublicationGate::evaluate(crate::review_gate::PublicationGateInput {
            recommendation,
            specialist_gate,
        });
    match gate.verdict {
        crate::review_gate::GateVerdict::Pass => (
            crate::github_feedback::gate_pass_comment_body(new_review_id, &gate.recommendation),
            None,
        ),
        crate::review_gate::GateVerdict::Warn | crate::review_gate::GateVerdict::Fail => {
            let failure = crate::github_feedback::gate_failure_from_publication_gate(
                new_review_id,
                &gate,
                meta,
            );
            (
                crate::github_feedback::gate_failure_comment_body(
                    new_review_id,
                    &gate.recommendation,
                    &failure,
                ),
                Some(failure),
            )
        }
    }
}

#[cfg(test)]
fn rereview_gate_comment_body_for_test(
    new_review_id: Uuid,
    recommendation: Option<&str>,
    meta: Option<&Value>,
) -> String {
    rereview_gate_comment_body(
        new_review_id,
        recommendation,
        meta,
        crate::review_gate::SpecialistGate::all_pass_for_test(),
    )
    .0
}

/// Fire-and-forget POST to `WEB_REVALIDATE_URL` so the Next.js ISR cache flips
/// the affected `/reviews/<id>` and `/` pages immediately. Used by both the
/// merge webhook (after `pr_open → published`) and `grokrxiv approve` (after
/// `awaiting_moderation → pr_open`). Silent no-op when the env isn't
/// configured — never an error path for the caller.
pub fn spawn_revalidate(state: &crate::state::AppState, review_id: uuid::Uuid) {
    let Some(url) = state.config.web_revalidate_url.as_deref() else {
        tracing::warn!(%review_id, "spawn_revalidate: WEB_REVALIDATE_URL unset; web cache not flushed");
        return;
    };
    let Some(secret) = state.config.revalidate_secret.as_deref() else {
        tracing::warn!(%review_id, "spawn_revalidate: REVALIDATE_SECRET unset; web cache not flushed");
        return;
    };
    let client = state.http.clone();
    let url = url.to_string();
    let secret = secret.to_string();
    tokio::spawn(async move {
        let res = client
            .post(&url)
            .header("x-revalidate-secret", secret)
            .json(&json!({
                "review_id": review_id,
                "paths": ["/", format!("/reviews/{}", review_id)],
            }))
            .send()
            .await;
        match res {
            Ok(r) => tracing::info!(status = %r.status(), %review_id, "revalidate ack"),
            Err(e) => tracing::warn!(err = %e, %review_id, "revalidate failed"),
        }
    });
}

/// Verify a GitHub `sha256=<hex>` signature against `body` using `secret`.
pub fn verify_signature(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&sig_bytes).is_ok()
}

/// Branches the publisher creates look like `review/<source-id>-<short-uuid>`.
/// V1 accepts arXiv ids plus local/git source ids made from safe ASCII
/// characters; the PR body marker remains the authoritative review id.
fn is_valid_review_branch(branch: &str) -> bool {
    let Some(rest) = branch.strip_prefix("review/") else {
        return false;
    };
    let Some((source_id, suffix)) = rest.rsplit_once('-') else {
        return false;
    };
    if suffix.is_empty() || suffix.len() > 16 || !suffix.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    !source_id.is_empty()
        && source_id.len() <= 96
        && source_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn extract_review_id_from_body(body: &str) -> Option<uuid::Uuid> {
    // The publisher embeds `grokrxiv-review-id: <uuid>` somewhere in the PR
    // body so we can correlate the merge back to a review row.
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("grokrxiv-review-id:") {
            if let Ok(id) = rest.trim().parse::<uuid::Uuid>() {
                return Some(id);
            }
        }
    }
    None
}

fn extract_correction_source_path_from_body(body: &str) -> Option<&str> {
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("grokrxiv-correction-source-path:") {
            let path = rest.trim();
            if !path.is_empty() && !path.starts_with('/') && !path.contains("..") {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_known_payload() {
        let secret = b"swordfish";
        let body = b"{\"hello\":\"world\"}";
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={sig}");
        assert!(verify_signature(secret, body, &header));
        assert!(!verify_signature(secret, body, "sha256=deadbeef"));
        assert!(!verify_signature(b"wrong", body, &header));
    }

    #[test]
    fn accepts_modern_review_branches() {
        assert!(is_valid_review_branch("review/2605.12484-a1b2c3d"));
        assert!(is_valid_review_branch("review/2401.12345v2-deadbee"));
    }

    #[test]
    fn accepts_legacy_arxiv_id_branches() {
        assert!(is_valid_review_branch("review/math.AG/0301001-abcd"));
        assert!(is_valid_review_branch("review/cs/9912345v1-feed"));
    }

    #[test]
    fn accepts_content_hash_source_branches() {
        assert!(is_valid_review_branch(
            "review/local-tex-a1b2c3d4e5f6-deadbee"
        ));
        assert!(is_valid_review_branch(
            "review/git-pdf-a1b2c3d4e5f6-feed1234"
        ));
    }

    #[test]
    fn rejects_malformed_branches() {
        assert!(!is_valid_review_branch("review/anything"));
        assert!(!is_valid_review_branch("review/2605.12484"));
        assert!(!is_valid_review_branch("review/2605.12484-"));
        assert!(!is_valid_review_branch("main"));
        assert!(!is_valid_review_branch("review/source id-abc"));
    }

    #[test]
    fn extracts_review_id_marker() {
        let body = "Closes #42\n\ngrokrxiv-review-id: 11111111-1111-1111-1111-111111111111\n";
        let id = extract_review_id_from_body(body).unwrap();
        assert_eq!(id.to_string(), "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn extracts_correction_source_marker() {
        let body = "\
Edit the manuscript snapshot.

grokrxiv-correction-source-path: corrections/source-1/paper.tex
grokrxiv-review-id: 11111111-1111-1111-1111-111111111111
";
        assert_eq!(
            extract_correction_source_path_from_body(body),
            Some("corrections/source-1/paper.tex")
        );
        assert_eq!(
            extract_review_id_from_body(body).map(|id| id.to_string()),
            Some("11111111-1111-1111-1111-111111111111".to_string())
        );
    }

    #[test]
    fn correction_source_marker_rejects_unsafe_paths() {
        assert_eq!(
            extract_correction_source_path_from_body(
                "grokrxiv-correction-source-path: /tmp/paper.tex"
            ),
            None
        );
        assert_eq!(
            extract_correction_source_path_from_body(
                "grokrxiv-correction-source-path: corrections/../paper.tex"
            ),
            None
        );
    }

    #[test]
    fn rereview_feedback_treats_minor_revision_as_failed_gate() {
        let body = rereview_gate_comment_body_for_test(
            uuid::Uuid::nil(),
            Some("minor_revision"),
            Some(&serde_json::json!({
                "summary": "Needs small fixes.",
                "weaknesses": ["citation context missing"],
                "questions": []
            })),
        );
        assert!(body.contains("Automated Review Gate: Failed"), "{body}");
        assert!(body.contains("minor_revision"), "{body}");
    }
}
