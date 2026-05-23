use crate::cli_status::StatusMark;
use crate::state::AppState;
use serde_json::json;

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn verify_artifact(
    state: &AppState,
    extract: &grokrxiv_schemas::PaperExtract,
    role: &str,
    artifact: &serde_json::Value,
) -> (
    Option<grokrxiv_schemas::VerifierStatus>,
    Option<serde_json::Value>,
) {
    use grokrxiv_schemas::VerifierStatus;
    use serde_json::json;

    let Some(ladder) = state.verifiers.get(role) else {
        return (None, None);
    };
    let ctx = grokrxiv_verifier::VerifierContext::for_paper(extract, &state.http);
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

pub(super) fn specialist_failure_output(role: &str, error: &str) -> serde_json::Value {
    match role {
        "summary" => summary_failure_output(error),
        "technical_correctness" => technical_failure_output(error),
        "novelty" => novelty_failure_output(error),
        "reproducibility" => reproducibility_failure_output(error),
        "citation" => citation_failure_output(error),
        _ => json!({
            "error": error,
            "role": role,
            "status": "agent_failed",
        }),
    }
}

fn summary_failure_output(error: &str) -> serde_json::Value {
    json!({
        "tldr": "Summary reviewer failed before producing a normal review.",
        "plain_language_summary": format!(
            "Automated summary generation failed before producing a normal review. Failure: {}",
            truncate_failure(error, 240)
        ),
        "key_contributions": [],
        "audience": null,
    })
}

fn technical_failure_output(error: &str) -> serde_json::Value {
    json!({
        "claims": [
            {
                "id": "technical_correctness_agent_failure",
                "claim": "Technical correctness reviewer failed before producing a normal review.",
                "location": null,
                "assessment": "partially_supported",
                "severity": "major",
                "evidence": format!("Failure: {}", truncate_failure(error, 240)),
                "suggested_fix": "Rerun automated review after the configured CLI/model provider recovers."
            }
        ],
        "overall_correctness": "questionable",
        "confidence": 0.0,
    })
}

fn novelty_failure_output(error: &str) -> serde_json::Value {
    json!({
        "novelty_score": 0.0,
        "related_work": [],
        "missing_prior_art": [
            {
                "title": "Novelty reviewer unavailable",
                "reason": format!(
                    "Automated novelty review failed before producing a normal prior-art assessment. Failure: {}",
                    truncate_failure(error, 240)
                )
            }
        ],
        "verdict": "marginal",
        "confidence": 0.0,
    })
}

fn reproducibility_failure_output(error: &str) -> serde_json::Value {
    json!({
        "code_availability": "unspecified",
        "code_url": null,
        "data_availability": "unspecified",
        "data_url": null,
        "environment": null,
        "concerns": [
            {
                "area": "other",
                "description": format!(
                    "Automated reproducibility review failed before producing a normal assessment. Failure: {}",
                    truncate_failure(error, 240)
                ),
                "severity": "major"
            }
        ],
        "reproducibility_score": 0.0,
        "confidence": 0.0,
    })
}

fn citation_failure_output(error: &str) -> serde_json::Value {
    json!({
        "entries": [],
        "missing_references": [],
        "summary": format!(
            "Citation-use agent failed before producing a normal citation relevance review. Deterministic citation verification still runs separately; see external citation checks and verifier provenance for existence, DOI, URL, and resolver evidence. Failure: {}",
            truncate_failure(error, 240)
        ),
        "confidence": 0.0,
    })
}

fn truncate_failure(error: &str, max_chars: usize) -> String {
    if error.chars().count() <= max_chars {
        return error.to_string();
    }
    format!("{}...", error.chars().take(max_chars).collect::<String>())
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

pub(super) fn role_status_label(role: &str) -> &str {
    role
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
    role: &str,
    output: &serde_json::Value,
    schemas: &crate::state::AgentSchemaMap,
) -> anyhow::Result<()> {
    let Some(schema) = schemas.get(role) else {
        return Ok(());
    };
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| anyhow::anyhow!("invalid schema for {role}: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(output)
        .map(|e| e.to_string())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "post-merge schema validation failed for {role}: {}",
            errors.join("; ")
        )
    }
}
