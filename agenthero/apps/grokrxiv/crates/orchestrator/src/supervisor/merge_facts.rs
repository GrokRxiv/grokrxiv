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
    mut output: serde_json::Value,
    v_notes: Option<&serde_json::Value>,
) -> serde_json::Value {
    if output.get("error").is_some() {
        return output;
    }
    annotate_degraded_citation_summary(&mut output, v_notes);
    populate_degraded_citation_entries(&mut output, v_notes);
    output
}

fn populate_degraded_citation_entries(
    output: &mut serde_json::Value,
    v_notes: Option<&serde_json::Value>,
) {
    if !is_degraded_citation_output(output) {
        return;
    }
    let Some(verifier_entries) = v_notes
        .and_then(citation_notes)
        .and_then(|notes| notes.get("entries"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    if verifier_entries.is_empty() {
        return;
    }
    let entries = verifier_entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| verifier_entry_to_citation_review_entry(idx, entry))
        .collect();
    if let Some(obj) = output.as_object_mut() {
        obj.insert("entries".to_string(), serde_json::Value::Array(entries));
    }
}

fn verifier_entry_to_citation_review_entry(
    idx: usize,
    entry: &serde_json::Value,
) -> serde_json::Value {
    let status = entry.get("status").and_then(serde_json::Value::as_str);
    let exists = entry
        .get("exists")
        .and_then(serde_json::Value::as_bool)
        .map(serde_json::Value::Bool)
        .or_else(|| match status {
            Some("resolved") => Some(serde_json::Value::Bool(true)),
            Some("unresolved") | Some("malformed") => Some(serde_json::Value::Bool(false)),
            _ => None,
        })
        .unwrap_or(serde_json::Value::Null);
    let key = entry
        .get("citation_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("ref{}", idx + 1));
    let reason = entry
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .filter(|reason| !reason.trim().is_empty());
    let source = entry
        .get("source")
        .and_then(serde_json::Value::as_str)
        .filter(|source| !source.trim().is_empty());
    let note = match (status, source, reason) {
        (Some(status), Some(source), Some(reason)) => {
            Some(format!("{status} via {source}: {reason}"))
        }
        (Some(status), Some(source), None) => Some(format!("{status} via {source}")),
        (Some(status), None, Some(reason)) => Some(format!("{status}: {reason}")),
        (None, Some(source), Some(reason)) => Some(format!("{source}: {reason}")),
        (Some(status), None, None) => Some(status.to_string()),
        (None, Some(source), None) => Some(source.to_string()),
        (None, None, Some(reason)) => Some(reason.to_string()),
        (None, None, None) => None,
    };
    serde_json::json!({
        "citation": {
            "key": key,
            "raw": entry.get("raw").cloned().unwrap_or(serde_json::Value::Null),
            "title": entry.get("title").cloned().unwrap_or(serde_json::Value::Null),
            "authors": [],
            "year": entry.get("year").cloned().unwrap_or(serde_json::Value::Null),
            "venue": serde_json::Value::Null,
            "doi": entry.get("doi").cloned().unwrap_or(serde_json::Value::Null),
            "arxiv_id": entry.get("arxiv_id").cloned().unwrap_or(serde_json::Value::Null),
            "url": entry.get("url").cloned().unwrap_or(serde_json::Value::Null),
        },
        "exists": exists,
        "resolved_doi": entry.get("resolved_doi").cloned().unwrap_or(serde_json::Value::Null),
        "resolved_url": entry.get("resolved_url").cloned().unwrap_or(serde_json::Value::Null),
        "relevance": "medium",
        "notes": note,
        "explanation": "Deterministic citation verifier result; citation-use agent timed out before relevance analysis.",
    })
}

fn annotate_degraded_citation_summary(
    output: &mut serde_json::Value,
    v_notes: Option<&serde_json::Value>,
) {
    let Some(checked) = v_notes.and_then(citation_checked_count) else {
        return;
    };
    if !is_degraded_citation_output(output) {
        return;
    }
    let Some(obj) = output.as_object_mut() else {
        return;
    };
    let Some(summary) = obj.get("summary").and_then(serde_json::Value::as_str) else {
        return;
    };
    if summary.contains("bibliography entries") {
        return;
    }
    obj.insert(
        "summary".to_string(),
        serde_json::Value::String(format!(
            "{summary} Deterministic citation verifier checked {checked} bibliography entries."
        )),
    );
}

fn is_degraded_citation_output(output: &serde_json::Value) -> bool {
    output
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|confidence| confidence == 0.0)
        && output
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
        && output
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|summary| summary.contains("Citation-use agent failed"))
}

fn citation_checked_count(v_notes: &serde_json::Value) -> Option<u64> {
    citation_notes(v_notes)
        .and_then(|notes| notes.get("checked"))
        .and_then(serde_json::Value::as_u64)
}

fn citation_notes(v_notes: &serde_json::Value) -> Option<&serde_json::Value> {
    v_notes
        .get("citation")
        .and_then(|citation| citation.get("notes"))
        .or_else(|| v_notes.get("notes"))
        .or(Some(v_notes))
}
