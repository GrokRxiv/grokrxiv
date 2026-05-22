pub(super) fn merge_novelty_facts_into_output(
    output: serde_json::Value,
    _facts: &crate::agents::review::facts::NoveltyFacts,
) -> serde_json::Value {
    output
}

/// Overlay verified URL / GitHub repo state onto the LLM's
/// `reproducibility_review`. Auto-adds a `concerns` entry for each
/// unreachable URL and each archived repo so the moderator UI surfaces the
/// gap regardless of whether the LLM noticed it. Existing concerns from the
/// LLM are preserved; we only append.
pub(super) fn merge_reproducibility_facts_into_output(
    mut output: serde_json::Value,
    facts: &crate::agents::review::facts::ReproducibilityFacts,
) -> serde_json::Value {
    let Some(obj) = output.as_object_mut() else {
        return output;
    };
    // Concerns array: append, don't clobber.
    let concerns = obj
        .entry("concerns".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(concerns_arr) = concerns.as_array_mut() else {
        return output;
    };
    use crate::agents::review::facts::UrlKind;
    for u in &facts.urls_checked {
        if u.reachable {
            continue;
        }
        if concern_mentions(concerns_arr, &u.url) {
            continue;
        }
        let area = match u.kind {
            UrlKind::Code => "code",
            UrlKind::Dataset => "data",
            UrlKind::Other => "other",
        };
        let severity = match u.kind {
            UrlKind::Code | UrlKind::Dataset => "major",
            UrlKind::Other => "minor",
        };
        let status = u
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "network_error".to_string());
        concerns_arr.push(serde_json::json!({
            "area": area,
            "description": format!("Verifier could not reach `{}` (status={})", u.url, status),
            "severity": severity,
        }));
    }
    for r in &facts.github_repos {
        if matches!(r.archived, Some(true)) {
            let repo = format!("{}/{}", r.owner, r.repo);
            let repo_needle = format!("GitHub repository `{repo}`");
            if concern_mentions(concerns_arr, &repo_needle) {
                continue;
            }
            concerns_arr.push(serde_json::json!({
                "area": "code",
                "description": format!(
                    "GitHub repository `{}/{}` is marked archived — code is no longer maintained.",
                    r.owner, r.repo
                ),
                "severity": "minor",
            }));
        }
    }
    output
}

fn concern_mentions(concerns: &[serde_json::Value], needle: &str) -> bool {
    concerns.iter().any(|concern| {
        concern
            .get("description")
            .and_then(|d| d.as_str())
            .map(|description| description.contains(needle))
            .unwrap_or(false)
    })
}

/// Keep verifier-owned citation resolution out of the LLM-authored citation
/// review JSON. Public/API consumers read definitive existence, DOI, URL, and
/// unknown/malformed status from `review_agents.verifier_notes`; the specialist
/// output remains a schema-valid citation-use review.
pub(super) fn merge_citation_verifier_into_output(
    output: serde_json::Value,
    _v_notes: Option<&serde_json::Value>,
) -> serde_json::Value {
    if output.get("error").is_some() {
        return output;
    }
    output
}
