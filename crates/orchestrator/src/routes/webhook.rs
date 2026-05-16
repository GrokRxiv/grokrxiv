//! `POST /webhook/github` — GitHub PR-merge webhook.
//!
//! Hardening:
//!
//! * Verifies the `X-Hub-Signature-256` HMAC against
//!   `GITHUB_WEBHOOK_SECRET`.
//! * Only acts on `pull_request.closed` events with `merged = true`.
//! * Verifies the merged branch name matches `review/<arxiv_id>-*`, the
//!   pattern the publisher creates. Anything else is ignored as 200 OK so
//!   the GitHub UI doesn't keep re-delivering.
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

    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
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

    // Revalidate the Vercel cache. The Next.js handler expects
    // `{ review_id: uuid, paths?: string[] }` and the handler itself appends
    // `/reviews/<id>` and `/` to its revalidation set — we send the explicit
    // `paths` anyway so the contract is self-describing.
    if let (Some(url), Some(secret)) = (
        state.config.web_revalidate_url.as_deref(),
        state.config.revalidate_secret.as_deref(),
    ) {
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

    (
        StatusCode::OK,
        Json(json!({ "ok": true, "review_id": review_id, "branch": branch })),
    )
        .into_response()
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

/// Branches the publisher creates look like `review/<arxiv_id>-<short-uuid>`.
/// We require the arXiv-id segment to match the modern format `YYMM.NNNNN[vN]`
/// or the legacy `subject-class/NNNNNNN` form and reject anything else. We
/// hand-roll the parsing to avoid a regex dependency on this hot path.
fn is_valid_review_branch(branch: &str) -> bool {
    let Some(rest) = branch.strip_prefix("review/") else {
        return false;
    };
    let Some((arxiv_id, suffix)) = rest.rsplit_once('-') else {
        return false;
    };
    if suffix.is_empty() || suffix.len() > 16 || !suffix.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    is_modern_arxiv_id(arxiv_id) || is_legacy_arxiv_id(arxiv_id)
}

fn is_modern_arxiv_id(id: &str) -> bool {
    // `YYMM.NNNNN` with optional `vN` suffix. YYMM is 4 digits, NNNNN is 4–5
    // digits.
    let (core, version_ok) = match id.find('v') {
        Some(idx) => {
            let v = &id[idx + 1..];
            (
                &id[..idx],
                !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()),
            )
        }
        None => (id, true),
    };
    if !version_ok {
        return false;
    }
    let Some((yymm, num)) = core.split_once('.') else {
        return false;
    };
    yymm.len() == 4
        && yymm.chars().all(|c| c.is_ascii_digit())
        && (num.len() == 4 || num.len() == 5)
        && num.chars().all(|c| c.is_ascii_digit())
}

fn is_legacy_arxiv_id(id: &str) -> bool {
    // `subject-class[.subcategory]/NNNNNNN[vN]` (e.g. `math.AG/0301001`).
    let (core, version_ok) = match id.find('v') {
        Some(idx) if idx > id.find('/').unwrap_or(0) => {
            let v = &id[idx + 1..];
            (
                &id[..idx],
                !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()),
            )
        }
        _ => (id, true),
    };
    if !version_ok {
        return false;
    }
    let Some((subject, num)) = core.split_once('/') else {
        return false;
    };
    if num.len() != 7 || !num.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Subject: lowercase letters / hyphens, optionally `.XX` subcategory.
    let (head, sub) = match subject.split_once('.') {
        Some((h, s)) => (h, Some(s)),
        None => (subject, None),
    };
    let head_ok = !head.is_empty() && head.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    let sub_ok = sub
        .map(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase()))
        .unwrap_or(true);
    head_ok && sub_ok
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
    fn rejects_malformed_branches() {
        assert!(!is_valid_review_branch("review/anything"));
        assert!(!is_valid_review_branch("review/2605.12484"));
        assert!(!is_valid_review_branch("review/2605.12484-"));
        assert!(!is_valid_review_branch("main"));
        assert!(!is_valid_review_branch("review/12-abc"));
    }

    #[test]
    fn extracts_review_id_marker() {
        let body = "Closes #42\n\ngrokrxiv-review-id: 11111111-1111-1111-1111-111111111111\n";
        let id = extract_review_id_from_body(body).unwrap();
        assert_eq!(id.to_string(), "11111111-1111-1111-1111-111111111111");
    }
}
