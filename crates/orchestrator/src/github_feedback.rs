//! GitHub feedback helpers for automated review-gate loops.

use anyhow::Result;
use serde_json::Value;
use uuid::Uuid;

use crate::state::AppState;

/// Structured failure text persisted in Postgres and mirrored into a stable
/// GitHub PR comment.
#[derive(Debug, Clone)]
pub struct GateFailureArtifact {
    /// Gate identifier.
    pub gate: String,
    /// Severity accepted by `review_gate_failures.severity`.
    pub severity: String,
    /// Short human-readable summary.
    pub summary: String,
    /// Markdown details for the author/moderator.
    pub details_md: String,
    /// Markdown action instructions.
    pub action_required_md: String,
}

/// Convert a meta-review recommendation into a durable gate-failure artifact.
pub fn gate_failure_from_meta(
    review_id: Uuid,
    recommendation: &str,
    meta: Option<&Value>,
) -> GateFailureArtifact {
    let summary =
        format!("Automated review gate failed: meta_reviewer recommended `{recommendation}`.");
    let meta_summary = meta
        .and_then(|m| m.get("summary"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("No meta-review summary was recorded.");
    let weaknesses = markdown_list(meta.and_then(|m| m.get("weaknesses")));
    let questions = markdown_list(meta.and_then(|m| m.get("questions")));
    let details_md = format!(
        "## Gate Result\n\n{summary}\n\n## Meta-review Summary\n\n{meta_summary}\n\n## Weaknesses\n\n{weaknesses}\n\n## Questions\n\n{questions}"
    );
    GateFailureArtifact {
        gate: "meta_reviewer_recommendation".to_string(),
        severity: if recommendation == "reject" {
            "critical".to_string()
        } else {
            "high".to_string()
        },
        summary,
        details_md,
        action_required_md: correction_loop_instructions(review_id),
    }
}

/// Convert a centralized publication-gate decision into durable feedback.
pub(crate) fn gate_failure_from_publication_gate(
    review_id: Uuid,
    gate: &crate::review_gate::PublicationGate,
    meta: Option<&Value>,
) -> GateFailureArtifact {
    let summary = format!(
        "Automated review gate failed: {}.",
        gate.reason.trim_end_matches('.')
    );
    let meta_summary = meta
        .and_then(|m| m.get("summary"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("No meta-review summary was recorded.");
    let weaknesses = markdown_list(meta.and_then(|m| m.get("weaknesses")));
    let questions = markdown_list(meta.and_then(|m| m.get("questions")));
    let details_md = format!(
        "## Gate Result\n\n{summary}\n\n## Meta-review Summary\n\n{meta_summary}\n\n## Weaknesses\n\n{weaknesses}\n\n## Questions\n\n{questions}"
    );
    GateFailureArtifact {
        gate: "publication_gate".to_string(),
        severity: if gate.recommendation == "reject" {
            "critical".to_string()
        } else {
            "high".to_string()
        },
        summary,
        details_md,
        action_required_md: correction_loop_instructions(review_id),
    }
}

/// Instructions shown in the public review details and GitHub feedback.
pub fn correction_loop_instructions(review_id: Uuid) -> String {
    let public_url =
        std::env::var("GROKRXIV_PUBLIC_URL").unwrap_or_else(|_| "https://grokrxiv.org".into());
    format!(
        "## How to Resubmit Corrections\n\n\
         1. Apply the requested fixes to the paper source on this PR branch.\n\
         2. Commit and push the correction back to GitHub:\n\n\
         ```bash\n\
         git status\n\
         git add <changed paper files>\n\
         git commit -m \"Address GrokRxiv review feedback\"\n\
         git push\n\
         ```\n\n\
         3. Each push to the PR triggers GrokRxiv automated re-review.\n\
         4. GrokRxiv updates the same GitHub feedback comment with pass/fail and the reason.\n\
         5. Continue this loop until the automated gate reports that the review passed.\n\n\
         Review details: {public_url}/reviews/{review_id}"
    )
}

/// Build the stable GitHub failure comment body.
pub fn gate_failure_comment_body(
    review_id: Uuid,
    recommendation: &str,
    failure: &GateFailureArtifact,
) -> String {
    let public_url =
        std::env::var("GROKRXIV_PUBLIC_URL").unwrap_or_else(|_| "https://grokrxiv.org".into());
    format!(
        "## GrokRxiv Automated Review Gate: Failed\n\n\
         Latest review: {public_url}/reviews/{review_id}\n\n\
         Recommendation: `{recommendation}`\n\n\
         {}\n\n\
         {}\n\n\
         {}",
        failure.summary, failure.details_md, failure.action_required_md
    )
}

/// Build the stable GitHub pass comment body.
pub fn gate_pass_comment_body(review_id: Uuid, recommendation: &str) -> String {
    let public_url =
        std::env::var("GROKRXIV_PUBLIC_URL").unwrap_or_else(|_| "https://grokrxiv.org".into());
    format!(
        "## GrokRxiv Automated Review Gate: Passed\n\n\
         Latest review: {public_url}/reviews/{review_id}\n\n\
         Recommendation: `{recommendation}`\n\n\
         The latest correction commit passed the automated review gate. No further automated corrections are required for this gate."
    )
}

/// Persist an automated gate failure row and return its id.
pub async fn record_gate_failure(
    state: &AppState,
    review_id: Uuid,
    failure: &GateFailureArtifact,
) -> Result<Option<Uuid>> {
    let Some(pool) = state.db.as_ref() else {
        return Ok(None);
    };
    let id = crate::db::insert_review_gate_failure(
        pool,
        review_id,
        &failure.gate,
        &failure.severity,
        &failure.summary,
        &failure.details_md,
        Some(&failure.action_required_md),
    )
    .await?;
    Ok(Some(id))
}

/// Create or update the stable gate-feedback comment on a PR.
#[cfg(feature = "grokrxiv-publisher")]
pub async fn post_or_update_gate_feedback_comment(
    state: &AppState,
    repo_owner: &str,
    repo_name: &str,
    pr_number: i64,
    marker_key: &str,
    body_md: &str,
) -> Result<Option<grokrxiv_publisher::GateFeedbackComment>> {
    let Some(token) = std::env::var("GITHUB_TOKEN").ok() else {
        tracing::warn!("GITHUB_TOKEN unset; skipping GitHub gate-feedback comment");
        return Ok(None);
    };
    let pr_number: u64 = pr_number.try_into()?;
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = grokrxiv_publisher::GithubPublisher::new(client, repo_owner, repo_name);
    let admin = grokrxiv_publisher::AdminCaller::from_admin_endpoint();
    let stable_marker = format!("<!-- grokrxiv:gate-feedback:{marker_key} -->");
    let comment = publisher
        .post_or_update_gate_feedback(&admin, pr_number, &stable_marker, body_md)
        .await?;
    if let Some(pool) = state.db.as_ref() {
        if let Ok(comment_id) = i64::try_from(comment.comment_id) {
            let _ = crate::db::update_github_feedback_comment(
                pool,
                marker_review_id(marker_key).unwrap_or(Uuid::nil()),
                comment_id,
                &comment.html_url,
            )
            .await;
        }
    }
    Ok(Some(comment))
}

/// Non-publisher builds cannot post GitHub comments.
#[cfg(not(feature = "grokrxiv-publisher"))]
pub async fn post_or_update_gate_feedback_comment(
    _state: &AppState,
    _repo_owner: &str,
    _repo_name: &str,
    _pr_number: i64,
    _marker_key: &str,
    _body_md: &str,
) -> Result<Option<()>> {
    Ok(None)
}

fn marker_review_id(marker_key: &str) -> Option<Uuid> {
    marker_key.strip_prefix("review-")?.parse().ok()
}

fn markdown_list(value: Option<&Value>) -> String {
    let Some(items) = value.and_then(Value::as_array) else {
        return "- None recorded.".to_string();
    };
    let lines: Vec<String> = items
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("- {}", s.trim()))
        .collect();
    if lines.is_empty() {
        "- None recorded.".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_gate_failure_summary_has_single_period() {
        let gate = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Fail,
            reason: "Meta-review recommendation is `major_revision`, not `accept`.".to_string(),
            recommendation: "major_revision".to_string(),
        };
        let failure = gate_failure_from_publication_gate(Uuid::nil(), &gate, None);
        assert_eq!(
            failure.summary,
            "Automated review gate failed: Meta-review recommendation is `major_revision`, not `accept`."
        );
    }
}
