use crate::cli_status::StatusMark;
use crate::state::AppState;
use serde_json::json;

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn verify_artifact(
    state: &AppState,
    extract: &grokrxiv_schemas::PaperExtract,
    role: grokrxiv_schemas::AgentRole,
    artifact: &serde_json::Value,
) -> (
    Option<grokrxiv_schemas::VerifierStatus>,
    Option<serde_json::Value>,
) {
    use grokrxiv_schemas::VerifierStatus;
    use serde_json::json;

    let Some(ladder) = state.verifiers.get(&role) else {
        return (None, None);
    };
    let ctx = grokrxiv_verifier::VerifierContext {
        paper: extract,
        http: &state.http,
    };
    let rungs: Vec<(String, grokrxiv_schemas::VerifierResult)> = ladder.run(artifact, &ctx).await;
    let worst = rungs
        .iter()
        .fold(VerifierStatus::Pass, |acc, (_, r)| match (acc, r.status) {
            (_, VerifierStatus::Fail) | (VerifierStatus::Fail, _) => VerifierStatus::Fail,
            (_, VerifierStatus::Warn) | (VerifierStatus::Warn, _) => VerifierStatus::Warn,
            _ => VerifierStatus::Pass,
        });
    let notes_obj: serde_json::Value = rungs
        .into_iter()
        .map(|(name, r)| {
            (
                name,
                json!({
                    "status": r.status,
                    "notes": r.notes,
                }),
            )
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();
    (Some(worst), Some(notes_obj))
}

pub(super) fn specialist_failure_output(
    role: grokrxiv_schemas::AgentRole,
    error: &str,
) -> serde_json::Value {
    json!({
        "error": error,
        "role": super::role_slug(role),
        "status": "agent_failed",
    })
}

pub(super) fn meta_failure_output(error: &str) -> serde_json::Value {
    json!({
        "summary": "Automated meta-review synthesis failed before producing a normal recommendation.",
        "strengths": [],
        "weaknesses": [
            format!("Meta-reviewer failed: {error}"),
        ],
        "questions": [
            "Please inspect the specialist outputs and rerun automated review after the CLI provider recovers.",
        ],
        "recommendation": "major_revision",
        "confidence": 1.0,
        "revision_targets": [],
    })
}

pub(super) fn role_status_label(role: grokrxiv_schemas::AgentRole) -> &'static str {
    match role {
        grokrxiv_schemas::AgentRole::Summary => "summary",
        grokrxiv_schemas::AgentRole::TechnicalCorrectness => "technical correctness",
        grokrxiv_schemas::AgentRole::Novelty => "novelty",
        grokrxiv_schemas::AgentRole::Reproducibility => "reproducibility",
        grokrxiv_schemas::AgentRole::Citation => "citation",
        grokrxiv_schemas::AgentRole::MetaReviewer => "meta reviewer",
    }
}

pub(super) fn verifier_status_mark(status: Option<grokrxiv_schemas::VerifierStatus>) -> StatusMark {
    match status {
        Some(grokrxiv_schemas::VerifierStatus::Pass) => StatusMark::Ok,
        Some(grokrxiv_schemas::VerifierStatus::Warn) => StatusMark::Warn,
        Some(grokrxiv_schemas::VerifierStatus::Fail) | None => StatusMark::Fail,
    }
}

#[cfg(feature = "grokrxiv-verifier")]
pub(super) fn validate_role_output_after_merge(
    role: grokrxiv_schemas::AgentRole,
    output: &serde_json::Value,
    schemas: &crate::state::AgentSchemaMap,
) -> anyhow::Result<()> {
    let Some(schema) = schemas.get(&role) else {
        return Ok(());
    };
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| anyhow::anyhow!("invalid schema for {role:?}: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(output)
        .map(|e| e.to_string())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "post-merge schema validation failed for {role:?}: {}",
            errors.join("; ")
        )
    }
}
