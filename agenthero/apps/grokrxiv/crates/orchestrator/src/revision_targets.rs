use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Debug, Clone)]
struct TargetCandidate {
    source_role: String,
    target_kind: &'static str,
    locator: Option<String>,
    evidence: Option<String>,
    required_update: String,
    verification_check: String,
    match_text: String,
}

pub(crate) fn enrich_meta_review(
    mut meta: Value,
    specialists: &Value,
    source_path_hint: Option<&str>,
) -> Value {
    let weaknesses = meta
        .get("weaknesses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let candidates = collect_candidates(specialists);
    let mut targets = Vec::new();

    for (index, weakness) in weaknesses.iter().enumerate() {
        let Some(text) = weakness.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let candidate = best_candidate(text, &candidates);
        let target = candidate
            .map(|candidate| target_from_candidate(index, text, candidate, source_path_hint))
            .unwrap_or_else(|| inferred_target(index, text, source_path_hint));
        targets.push(target);
    }

    if let Some(obj) = meta.as_object_mut() {
        obj.insert("revision_targets".to_string(), Value::Array(targets));
    }
    meta
}

pub(crate) fn reconcile_revision_targets(prior_meta: Option<&Value>, mut new_meta: Value) -> Value {
    let Some(prior_targets) = prior_meta
        .and_then(|meta| meta.get("revision_targets"))
        .and_then(Value::as_array)
    else {
        return new_meta;
    };
    if prior_targets.is_empty() {
        return new_meta;
    }
    let new_targets = new_meta
        .get("revision_targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if new_targets.is_empty() {
        return new_meta;
    }

    let mut used_new = vec![false; new_targets.len()];
    let mut reconciled = Vec::new();
    for prior in prior_targets {
        if let Some(index) = new_targets.iter().enumerate().find_map(|(index, target)| {
            (!used_new[index] && targets_match(prior, target)).then_some(index)
        }) {
            used_new[index] = true;
            let mut still_open = new_targets[index].clone();
            set_status(&mut still_open, "still_open");
            if still_open.get("id").and_then(Value::as_str).is_none() {
                if let Some(id) = prior.get("id").and_then(Value::as_str) {
                    set_string(&mut still_open, "id", id);
                }
            }
            reconciled.push(still_open);
        } else {
            let mut addressed = prior.clone();
            set_status(&mut addressed, "addressed");
            reconciled.push(addressed);
        }
    }

    for (index, target) in new_targets.into_iter().enumerate() {
        if !used_new[index] {
            let mut open = target;
            set_status(&mut open, "open");
            reconciled.push(open);
        }
    }

    if let Some(obj) = new_meta.as_object_mut() {
        obj.insert("revision_targets".to_string(), Value::Array(reconciled));
    }
    new_meta
}

pub(crate) fn revision_targets_markdown(meta: Option<&Value>) -> String {
    let Some(targets) = meta
        .and_then(|m| m.get("revision_targets"))
        .and_then(Value::as_array)
    else {
        return "- None recorded.".to_string();
    };
    let lines: Vec<String> = targets.iter().filter_map(format_target_markdown).collect();
    if lines.is_empty() {
        "- None recorded.".to_string()
    } else {
        lines.join("\n")
    }
}

pub(crate) fn revision_dependency_graph_markdown(meta: Option<&Value>) -> String {
    let Some(targets) = meta
        .and_then(|m| m.get("revision_targets"))
        .and_then(Value::as_array)
    else {
        return "- None recorded.".to_string();
    };
    let nodes: Vec<DependencyNode> = targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| dependency_node(index, target))
        .collect();
    if nodes.is_empty() {
        return "- None recorded.".to_string();
    }
    let edges = dependency_edges(&nodes);
    if edges.is_empty() {
        return "- No cross-target dependencies detected; these revision targets can be addressed in parallel.".to_string();
    }

    let mut lines = vec!["```mermaid".to_string(), "flowchart TD".to_string()];
    for node in &nodes {
        lines.push(format!(
            "  {}[\"{}\"]",
            node.graph_id,
            escape_mermaid_label(&node.label)
        ));
    }
    for edge in &edges {
        lines.push(format!(
            "  {} --> {}",
            nodes[edge.from].graph_id, nodes[edge.to].graph_id
        ));
    }
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("Remediation order:".to_string());
    for (index, node) in nodes.iter().enumerate() {
        let deps: Vec<&DependencyNode> = edges
            .iter()
            .filter(|edge| edge.to == index)
            .map(|edge| &nodes[edge.from])
            .collect();
        if deps.is_empty() {
            lines.push(format!(
                "- `{}` can start immediately.",
                node.display_id.as_str()
            ));
        } else {
            let dep_list = deps
                .iter()
                .map(|dep| format!("`{}`", dep.display_id))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- `{}` should follow {dep_list}.",
                node.display_id.as_str()
            ));
        }
    }
    lines.join("\n")
}

#[derive(Debug)]
struct DependencyNode {
    display_id: String,
    graph_id: String,
    label: String,
    kind: String,
    text: String,
}

#[derive(Debug)]
struct DependencyEdge {
    from: usize,
    to: usize,
}

fn dependency_node(index: usize, target: &Value) -> Option<DependencyNode> {
    let required_update = str_field(target, "required_update")?;
    if required_update.trim().is_empty() {
        return None;
    }
    let status = str_field(target, "status").unwrap_or_else(|| "open".to_string());
    if matches!(status.as_str(), "addressed" | "superseded") {
        return None;
    }
    let id = str_field(target, "id").unwrap_or_else(|| format!("target-{}", index + 1));
    let kind = str_field(target, "target_kind").unwrap_or_else(|| "unknown".to_string());
    let locator = str_field(target, "locator").filter(|s| !s.trim().is_empty());
    let evidence = str_field(target, "evidence").unwrap_or_default();
    let label = format!(
        "{}: {}",
        id,
        target_heading(&kind, locator.as_deref(), &required_update)
    );
    let text = join_match_text([
        required_update,
        locator.unwrap_or_default(),
        evidence,
        str_field(target, "verification_check").unwrap_or_default(),
    ])
    .to_ascii_lowercase();
    Some(DependencyNode {
        display_id: id.clone(),
        graph_id: graph_node_id(&id, index),
        label: short_text(&label, 96),
        kind,
        text,
    })
}

fn dependency_edges(nodes: &[DependencyNode]) -> Vec<DependencyEdge> {
    let mut edges = Vec::new();
    for (from, upstream) in nodes.iter().enumerate() {
        for (to, target) in nodes.iter().enumerate() {
            if from == to || !target_depends_on(target, upstream) {
                continue;
            }
            edges.push(DependencyEdge { from, to });
        }
    }
    edges
}

fn target_depends_on(target: &DependencyNode, upstream: &DependencyNode) -> bool {
    match (target.kind.as_str(), upstream.kind.as_str()) {
        ("paper_tex" | "paper_pdf", "bibliography") => mentions_any(
            &target.text,
            &["citation", "cite", "reference", "bibliography", "prior art"],
        ),
        ("paper_tex" | "paper_pdf", "code") => mentions_any(
            &target.text,
            &[
                "artifact",
                "benchmark",
                "code",
                "evaluation",
                "experiment",
                "formal",
                "machine-checkable",
                "model",
                "proof",
                "protocol",
                "quantitative",
                "reproducible",
                "script",
                "simulation",
                "statistical",
                "speed-up",
                "speedup",
            ],
        ),
        ("paper_tex" | "paper_pdf", "data") => mentions_any(
            &target.text,
            &[
                "benchmark",
                "corpus",
                "csv",
                "data",
                "dataset",
                "evaluation",
                "statistical",
                "taxonomy",
                "validation",
            ],
        ),
        ("code", "data") => mentions_any(
            &target.text,
            &[
                "benchmark",
                "bootstrap",
                "data",
                "dataset",
                "evaluation",
                "hac",
                "pipeline",
                "statistical",
                "validation",
            ],
        ),
        ("code", "code") => {
            mentions_any(&upstream.text, &["entrypoint", "source code", "release"])
                && mentions_any(
                    &target.text,
                    &[
                        "benchmark",
                        "evaluation",
                        "pipeline",
                        "statistical",
                        "validation",
                    ],
                )
        }
        ("review_text", _) => true,
        _ => false,
    }
}

fn mentions_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn graph_node_id(id: &str, index: usize) -> String {
    let mut out = String::from("n");
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out == "n" {
        out.push_str(&(index + 1).to_string());
    }
    out
}

/// Sanitize a Mermaid `flowchart` node label for GitHub's renderer. GitHub's Mermaid does
/// NOT support backslash-escaped quotes inside `id["…"]` (the `\"` ends the quoted string
/// early → `Parse error … got 'STR'`). Use Mermaid entity codes (`#NN;` / `#quot;`) for the
/// characters that break the parser, and collapse newlines, so labels like
/// `…(Construction 55 "symmofhopf")` render instead of crashing the whole diagram.
fn escape_mermaid_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        match ch {
            '"' => out.push_str("#quot;"),
            '(' => out.push_str("#40;"),
            ')' => out.push_str("#41;"),
            '[' => out.push_str("#91;"),
            ']' => out.push_str("#93;"),
            '{' => out.push_str("#123;"),
            '}' => out.push_str("#125;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '\\' => out.push_str("#92;"),
            '\r' | '\n' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

fn collect_candidates(specialists: &Value) -> Vec<TargetCandidate> {
    let root = specialists.get("specialists").unwrap_or(specialists);
    let mut out = Vec::new();
    let Some(roles) = root.as_object() else {
        return out;
    };
    for (source_role, value) in roles {
        collect_technical(source_role, Some(value), &mut out);
        collect_reproducibility(source_role, Some(value), &mut out);
        collect_novelty(source_role, Some(value), &mut out);
        collect_citation(source_role, Some(value), &mut out);
    }
    out
}

fn collect_technical(source_role: &str, value: Option<&Value>, out: &mut Vec<TargetCandidate>) {
    let Some(claims) = value
        .and_then(|v| v.get("claims"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for claim in claims {
        let assessment = str_field(claim, "assessment").unwrap_or_default();
        let severity = str_field(claim, "severity").unwrap_or_default();
        if assessment == "supported" && !matches!(severity.as_str(), "minor" | "major" | "critical")
        {
            continue;
        }
        let claim_text = str_field(claim, "claim").unwrap_or_default();
        let evidence = str_field(claim, "evidence")
            .filter(|s| !s.is_empty())
            .or_else(|| (!claim_text.is_empty()).then(|| claim_text.clone()));
        let suggested = str_field(claim, "suggested_fix").filter(|s| !s.is_empty());
        let required_update = suggested
            .clone()
            .unwrap_or_else(|| format!("Revise or justify this claim: {claim_text}"));
        let locator = str_field(claim, "location").filter(|s| !s.is_empty());
        let verification_check = locator
            .as_deref()
            .map(|loc| format!("Re-review should confirm `{loc}` is corrected or justified."))
            .unwrap_or_else(|| {
                "Re-review should confirm the affected claim is corrected or justified.".to_string()
            });
        out.push(TargetCandidate {
            source_role: source_role.to_string(),
            target_kind: "paper_tex",
            locator,
            evidence,
            required_update: required_update.clone(),
            verification_check,
            match_text: join_match_text([claim_text, required_update]),
        });
    }
}

fn collect_reproducibility(
    source_role: &str,
    value: Option<&Value>,
    out: &mut Vec<TargetCandidate>,
) {
    let Some(concerns) = value
        .and_then(|v| v.get("concerns"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for concern in concerns {
        let description = str_field(concern, "description").unwrap_or_default();
        if description.is_empty() {
            continue;
        }
        let area = str_field(concern, "area").unwrap_or_else(|| "other".to_string());
        let target_kind = match area.as_str() {
            "code" | "compute" | "hyperparameters" | "evaluation" => "code",
            "data" => "data",
            _ => "paper_tex",
        };
        let locator = reproducibility_locator(&area);
        let required_update = reproducibility_required_update(&area, &description);
        let verification_check = reproducibility_verification_check(&area);
        out.push(TargetCandidate {
            source_role: source_role.to_string(),
            target_kind,
            locator: Some(locator),
            evidence: Some(description.clone()),
            required_update: required_update.clone(),
            verification_check,
            match_text: join_match_text([area, description, required_update]),
        });
    }
}

fn reproducibility_locator(area: &str) -> String {
    match area {
        "data" => "data availability and restricted inputs",
        "compute" => "compute requirements and runnable smoke path",
        "hyperparameters" => "experiment configuration",
        "evaluation" => "evaluation and statistical-testing pipeline",
        "code" => "code release and execution entrypoints",
        _ => "reproducibility appendix",
    }
    .to_string()
}

fn reproducibility_required_update(area: &str, description: &str) -> String {
    match area {
        "data" => "Add a frozen data snapshot or a reproducible data-access appendix covering price series, historical index membership, data URLs, licenses, and restricted Bloomberg access constraints.".to_string(),
        "compute" => "Document the hardware, expected runtime per training cycle, and a reduced smoke configuration or checkpoint path that lets reviewers validate the pipeline without rerunning the full training workload.".to_string(),
        "hyperparameters" => "Publish the exact experiment configuration: random seeds, candidate grids, fold boundaries/calendars, initialization policy, package versions, top-k choices, and penalty settings.".to_string(),
        "evaluation" => "Publish evaluation scripts or executable pseudocode for walk-forward retraining, validation selection, HAC/bootstrap settings, benchmark construction, and table regeneration.".to_string(),
        "code" => "Release the source code, scripts, model configuration, and execution entrypoints needed to regenerate the reported tables, or document why those artifacts cannot be released.".to_string(),
        _ => format!("Add a reproducibility note that resolves this concern: {description}"),
    }
}

fn reproducibility_verification_check(area: &str) -> String {
    match area {
        "data" => "Re-review should find data artifacts or access instructions sufficient to rebuild the reported tables.",
        "compute" => "Re-review should confirm compute requirements and a smaller validation path are documented.",
        "hyperparameters" => "Re-review should confirm exact seeds, grids, folds, package versions, and configuration choices are specified.",
        "evaluation" => "Re-review should confirm the evaluation pipeline can reproduce the reported statistical tests and benchmarks.",
        "code" => "Re-review should confirm runnable code or a documented non-release justification is present.",
        _ => "Re-review should confirm the reproducibility concern is addressed with a concrete artifact or manuscript update.",
    }
    .to_string()
}

fn collect_novelty(source_role: &str, value: Option<&Value>, out: &mut Vec<TargetCandidate>) {
    let Some(items) = value
        .and_then(|v| v.get("missing_prior_art"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for item in items {
        let title = str_field(item, "title").unwrap_or_default();
        let reason = str_field(item, "reason").unwrap_or_default();
        if title.is_empty() && reason.is_empty() {
            continue;
        }
        let required_update = if title.is_empty() {
            format!("Discuss missing prior art: {reason}")
        } else {
            format!("Add or discuss missing prior art `{title}`. {reason}")
        };
        out.push(TargetCandidate {
            source_role: source_role.to_string(),
            target_kind: "bibliography",
            locator: (!title.is_empty()).then_some(title.clone()),
            evidence: (!reason.is_empty()).then_some(reason.clone()),
            required_update: required_update.clone(),
            verification_check:
                "Re-review should confirm the related-work discussion addresses this prior art."
                    .to_string(),
            match_text: join_match_text([title, reason, required_update]),
        });
    }
}

fn collect_citation(source_role: &str, value: Option<&Value>, out: &mut Vec<TargetCandidate>) {
    if let Some(items) = value
        .and_then(|v| v.get("missing_references"))
        .and_then(Value::as_array)
    {
        for item in items {
            let title = str_field(item, "title").unwrap_or_default();
            let reason = str_field(item, "reason").unwrap_or_default();
            if title.is_empty() && reason.is_empty() {
                continue;
            }
            let required_update = if title.is_empty() {
                format!("Add the missing reference or explain why it is out of scope: {reason}")
            } else {
                format!(
                    "Add a bibliography entry for `{}` and cite it where the affected method or claim is introduced, or explicitly justify its omission.",
                    clean_trailing_period(&title)
                )
            };
            out.push(TargetCandidate {
                source_role: source_role.to_string(),
                target_kind: "bibliography",
                locator: (!title.is_empty()).then_some(title.clone()),
                evidence: (!reason.is_empty()).then_some(reason.clone()),
                required_update: required_update.clone(),
                verification_check: "Re-review should confirm the bibliography and citation context address this reference.".to_string(),
                match_text: join_match_text([title, reason, required_update]),
            });
        }
    }

    let Some(entries) = value
        .and_then(|v| v.get("entries"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for entry in entries {
        let exists = entry.get("exists").and_then(Value::as_bool);
        let relevance = str_field(entry, "relevance").unwrap_or_default();
        if exists != Some(false) && !matches!(relevance.as_str(), "low" | "unrelated") {
            continue;
        }
        let citation = entry.get("citation").unwrap_or(&Value::Null);
        let key = str_field(citation, "key");
        let title = str_field(citation, "title");
        let raw = str_field(citation, "raw");
        let notes = str_field(entry, "notes")
            .or_else(|| str_field(entry, "explanation"))
            .unwrap_or_default();
        let locator = key.or(title).or(raw);
        let required_update = locator
            .as_deref()
            .map(|loc| {
                format!(
                    "Verify `{}` against an authoritative source; replace it with a resolvable relevant citation or remove it.",
                    clean_trailing_period(loc)
                )
            })
            .unwrap_or_else(|| {
                "Verify the unresolved citation against an authoritative source; replace it with a resolvable relevant citation or remove it.".to_string()
            });
        out.push(TargetCandidate {
            source_role: source_role.to_string(),
            target_kind: "bibliography",
            locator: locator.clone(),
            evidence: (!notes.is_empty()).then_some(notes.clone()),
            required_update: required_update.clone(),
            verification_check: "Re-review should confirm the citation resolves and is relevant."
                .to_string(),
            match_text: join_match_text([locator.unwrap_or_default(), notes, required_update]),
        });
    }
}

fn best_candidate<'a>(
    weakness: &str,
    candidates: &'a [TargetCandidate],
) -> Option<&'a TargetCandidate> {
    candidates
        .iter()
        .map(|candidate| (candidate_score(weakness, candidate), candidate))
        .filter(|(score, _)| *score >= 2)
        .max_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate)
}

fn candidate_score(weakness: &str, candidate: &TargetCandidate) -> usize {
    let weakness_lower = weakness.to_ascii_lowercase();
    let mut score = 0;
    if let Some(locator) = candidate.locator.as_deref() {
        let locator_lower = locator.to_ascii_lowercase();
        if !locator_lower.is_empty() && weakness_lower.contains(&locator_lower) {
            score += 8;
        }
    }
    if infer_target_kind(weakness) == candidate.target_kind {
        score += 2;
    }
    let weakness_tokens = tokens(weakness);
    let candidate_tokens = tokens(&candidate.match_text);
    score + weakness_tokens.intersection(&candidate_tokens).count()
}

fn target_from_candidate(
    index: usize,
    weakness: &str,
    candidate: &TargetCandidate,
    source_path_hint: Option<&str>,
) -> Value {
    json!({
        "id": format!("weakness-{}", index + 1),
        "weakness_index": index,
        "source_role": candidate.source_role.as_str(),
        "target_kind": candidate.target_kind,
        "source_path": source_path_for(candidate.target_kind, source_path_hint, weakness, &candidate.required_update),
        "locator": candidate.locator,
        "evidence": candidate.evidence,
        "required_update": candidate.required_update,
        "verification_check": candidate.verification_check,
        "status": "open"
    })
}

fn inferred_target(index: usize, weakness: &str, source_path_hint: Option<&str>) -> Value {
    let target_kind = infer_target_kind(weakness);
    let locator = infer_locator(weakness);
    json!({
        "id": format!("weakness-{}", index + 1),
        "weakness_index": index,
        "source_role": null,
        "target_kind": target_kind,
        "source_path": source_path_for(target_kind, source_path_hint, weakness, weakness),
        "locator": locator,
        "evidence": weakness,
        "required_update": weakness,
        "verification_check": verification_check_for(target_kind, locator.as_deref()),
        "status": "open"
    })
}

fn source_path_for(
    target_kind: &str,
    source_path_hint: Option<&str>,
    weakness: &str,
    required_update: &str,
) -> Option<String> {
    if matches!(target_kind, "paper_tex" | "paper_pdf") {
        return source_path_hint.map(str::to_string);
    }
    if target_kind == "code" {
        return extract_path_like(required_update).or_else(|| extract_path_like(weakness));
    }
    None
}

fn infer_target_kind(weakness: &str) -> &'static str {
    let lower = weakness.to_ascii_lowercase();
    if lower.contains("bibliograph")
        || lower.contains("citation")
        || lower.contains("reference")
        || lower.contains("prior art")
    {
        "bibliography"
    } else if lower.contains("source code")
        || lower.contains("script")
        || lower.contains("checkpoint")
        || lower.contains("seed")
        || lower.contains("hyperparameter")
        || lower.contains("python")
        || lower.contains("library")
    {
        "code"
    } else if lower.contains("dataset")
        || lower.contains("data ")
        || lower.contains("bloomberg")
        || lower.contains("index membership")
        || lower.contains("benchmark")
        || lower.contains("currency")
        || lower.contains("etf")
    {
        "data"
    } else if lower.contains("multiple-testing")
        || lower.contains("multiple testing")
        || lower.contains("bonferroni")
        || lower.contains("romano-wolf")
        || lower.contains("fdr")
        || lower.contains("hac")
        || lower.contains("abnormal return")
        || lower.contains("sharpe")
        || lower.contains("information ratio")
        || lower.contains("statistical")
    {
        "paper_tex"
    } else if lower.contains("review") && lower.contains("wording") {
        "review_text"
    } else if lower.contains("pdf") {
        "paper_pdf"
    } else if lower.contains("equation")
        || lower.contains("eq:")
        || lower.contains("section")
        || lower.contains("table")
        || lower.contains("formula")
        || lower.contains("novelty")
        || lower.contains("contribution")
        || lower.contains("incremental")
    {
        "paper_tex"
    } else {
        "unknown"
    }
}

fn infer_locator(weakness: &str) -> Option<String> {
    find_marker(weakness, "eq:")
        .or_else(|| find_marker(weakness, "Equation "))
        .or_else(|| find_marker(weakness, "equation "))
        .or_else(|| find_marker(weakness, "Table "))
        .or_else(|| find_marker(weakness, "table "))
        .or_else(|| find_marker(weakness, "Section "))
        .or_else(|| find_marker(weakness, "section "))
        .map(str::to_string)
        .or_else(|| infer_topic_locator(weakness))
}

fn infer_topic_locator(weakness: &str) -> Option<String> {
    let lower = weakness.to_ascii_lowercase();
    if lower.contains("multiple-testing")
        || lower.contains("multiple testing")
        || lower.contains("bonferroni")
        || lower.contains("romano-wolf")
        || lower.contains("fdr")
    {
        Some("multiple-testing correction".to_string())
    } else if lower.contains("sharpe") || lower.contains("information ratio") {
        Some("Sharpe and information-ratio significance tests".to_string())
    } else if lower.contains("abnormal return") || lower.contains("hac") {
        Some("abnormal returns significance tests".to_string())
    } else if lower.contains("benchmark") || lower.contains("etf") || lower.contains("currency") {
        Some("benchmark/data section".to_string())
    } else if lower.contains("hyperparameter")
        || lower.contains("reward scaling")
        || lower.contains("entropy coefficient")
    {
        Some("SAC hyperparameters and reward scaling".to_string())
    } else if lower.contains("novelty")
        || lower.contains("contribution")
        || lower.contains("incremental")
    {
        Some("contribution and novelty framing".to_string())
    } else {
        None
    }
}

fn find_marker<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let start = text.find(marker)?;
    let tail = &text[start..];
    let end = tail
        .char_indices()
        .take_while(|(_, c)| {
            c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.' | '/' | ' ')
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(marker.len());
    Some(tail[..end].trim())
}

fn verification_check_for(target_kind: &str, locator: Option<&str>) -> String {
    match (target_kind, locator) {
        ("paper_tex", Some(locator)) | ("paper_pdf", Some(locator)) => {
            format!("Re-review should confirm `{locator}` has been updated and the affected claims are consistent.")
        }
        ("paper_tex", None) | ("paper_pdf", None) => {
            "Re-review should confirm the manuscript text has been updated and the affected claims are consistent.".to_string()
        }
        ("code", _) => {
            "Re-review should confirm the code, scripts, seeds, environment, or checkpoints are available and documented.".to_string()
        }
        ("data", _) => {
            "Re-review should confirm the data source, access constraints, and evaluation setup are documented or corrected.".to_string()
        }
        ("bibliography", _) => {
            "Re-review should confirm the bibliography and related-work discussion address this item.".to_string()
        }
        ("review_text", _) => {
            "Re-review should confirm the public review text no longer contains this issue.".to_string()
        }
        _ => "Re-review should confirm this weakness has been addressed or justified.".to_string(),
    }
}

fn targets_match(a: &Value, b: &Value) -> bool {
    let a_kind = str_field(a, "target_kind").unwrap_or_default();
    let b_kind = str_field(b, "target_kind").unwrap_or_default();
    let a_locator = str_field(a, "locator").unwrap_or_default();
    let b_locator = str_field(b, "locator").unwrap_or_default();
    if !a_kind.is_empty()
        && a_kind == b_kind
        && !a_locator.is_empty()
        && a_locator.eq_ignore_ascii_case(&b_locator)
    {
        return true;
    }
    let a_text = join_match_text([
        str_field(a, "required_update").unwrap_or_default(),
        str_field(a, "evidence").unwrap_or_default(),
    ]);
    let b_text = join_match_text([
        str_field(b, "required_update").unwrap_or_default(),
        str_field(b, "evidence").unwrap_or_default(),
    ]);
    tokens(&a_text).intersection(&tokens(&b_text)).count() >= 4
}

fn format_target_markdown(target: &Value) -> Option<String> {
    let required_update = str_field(target, "required_update")?;
    if required_update.trim().is_empty() {
        return None;
    }
    let status = str_field(target, "status").unwrap_or_else(|| "open".to_string());
    let kind = str_field(target, "target_kind").unwrap_or_else(|| "unknown".to_string());
    let locator = str_field(target, "locator").filter(|s| !s.trim().is_empty());
    let source_path = str_field(target, "source_path").filter(|s| !s.trim().is_empty());
    let evidence = str_field(target, "evidence").filter(|s| s.trim() != required_update.trim());
    let verification_check = str_field(target, "verification_check").unwrap_or_default();
    let heading = target_heading(&kind, locator.as_deref(), &required_update);
    let mut lines = vec![format!(
        "- {} **{}**{}",
        status_checkbox(&status),
        heading,
        status_suffix(&status)
    )];
    lines.push(format!(
        "  - Location: {}",
        target_location(&kind, source_path.as_deref(), locator.as_deref())
    ));
    if let Some(evidence) = evidence {
        lines.push(format!("  - Evidence: {evidence}"));
    }
    lines.push(format!("  - Required change: {required_update}"));
    if !verification_check.is_empty() {
        lines.push(format!("  - Verification: {verification_check}"));
    }
    Some(lines.join("\n"))
}

fn status_checkbox(status: &str) -> &'static str {
    match status {
        "addressed" => "[x]",
        _ => "[ ]",
    }
}

fn status_suffix(status: &str) -> String {
    match status {
        "open" => String::new(),
        other => format!(" _({other})_"),
    }
}

fn target_heading(kind: &str, locator: Option<&str>, required_update: &str) -> String {
    match kind {
        "data" => {
            if locator
                .map(|loc| loc.contains("restricted") || loc.contains("data availability"))
                .unwrap_or(false)
            {
                "Data availability and restricted inputs".to_string()
            } else {
                format!(
                    "Data target: {}",
                    short_text(locator.unwrap_or(required_update), 80)
                )
            }
        }
        "code" => match locator.unwrap_or_default() {
            loc if loc.contains("compute") => "Compute reproducibility".to_string(),
            loc if loc.contains("configuration") => "Experiment configuration".to_string(),
            loc if loc.contains("evaluation") => "Evaluation pipeline".to_string(),
            loc if loc.contains("entrypoints") => "Code release and entrypoints".to_string(),
            loc if loc.contains("SAC hyperparameters") => {
                "SAC hyperparameters and reward scaling".to_string()
            }
            loc if !loc.is_empty() => {
                format!("Code/reproducibility target: {}", short_text(loc, 80))
            }
            _ => "Code/reproducibility artifacts".to_string(),
        },
        "bibliography" => format!(
            "Bibliography: {}",
            short_text(locator.unwrap_or(required_update), 96)
        ),
        "paper_tex" | "paper_pdf" => {
            if let Some(locator) = locator {
                format!("Manuscript: {}", short_text(locator, 96))
            } else {
                format!("Manuscript: {}", short_text(required_update, 96))
            }
        }
        "review_text" => "Review text correction".to_string(),
        _ => format!("Revision target: {}", short_text(required_update, 96)),
    }
}

fn target_location(kind: &str, source_path: Option<&str>, locator: Option<&str>) -> String {
    match (source_path, locator) {
        (Some(path), Some(locator)) => format!("`{path}` at `{locator}`"),
        (Some(path), None) => format!("`{path}`"),
        (None, Some(locator)) if kind == "data" => {
            format!("data/reproducibility artifacts: `{locator}`")
        }
        (None, Some(locator)) if kind == "code" => {
            format!("code/reproducibility artifacts: `{locator}`")
        }
        (None, Some(locator)) if kind == "bibliography" => {
            format!("bibliography entry: `{}`", short_text(locator, 120))
        }
        (None, Some(locator)) => format!("`{locator}`"),
        (None, None) if kind == "data" => "data/reproducibility artifacts".to_string(),
        (None, None) if kind == "code" => "code/reproducibility artifacts".to_string(),
        (None, None) if kind == "bibliography" => "bibliography".to_string(),
        _ => "review artifact".to_string(),
    }
}

fn clean_trailing_period(text: &str) -> String {
    text.trim().trim_end_matches('.').to_string()
}

fn short_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push_str("...");
    out
}

fn set_status(value: &mut Value, status: &str) {
    set_string(value, "status", status);
}

fn set_string(value: &mut Value, key: &str, text: &str) {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(key.to_string(), Value::String(text.to_string()));
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn tokens(text: &str) -> HashSet<String> {
    const STOP: &[&str] = &[
        "about", "after", "also", "because", "been", "being", "could", "from", "given", "into",
        "must", "that", "their", "there", "this", "under", "with", "without", "would",
    ];
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 4 && !STOP.contains(&s.as_str()))
        .collect()
}

fn join_match_text(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_path_like(text: &str) -> Option<String> {
    for raw in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | '(')) {
        let token = raw.trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '.'));
        if token.contains('/')
            || token.ends_with(".py")
            || token.ends_with(".rs")
            || token.ends_with(".jl")
            || token.ends_with(".ipynb")
            || token.ends_with(".lean")
            || token.ends_with(".tex")
        {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn technical_claim_location_becomes_revision_target() {
        let meta = json!({
            "summary": "Needs work.",
            "strengths": [],
            "weaknesses": ["The Maximum Drawdown formula (eq:md) is mathematically incorrect."],
            "questions": [],
            "recommendation": "major_revision",
            "confidence": 0.9
        });
        let specialists = json!({
            "specialists": {
                "technical_correctness": {
                    "claims": [{
                        "claim": "Maximum drawdown formula is incorrect",
                        "location": "eq:md",
                        "assessment": "incorrect",
                        "severity": "major",
                        "evidence": "R_i,T is independent of the optimization indices.",
                        "suggested_fix": "Correct the drawdown formula and verify affected table values."
                    }]
                }
            }
        });
        let enriched = enrich_meta_review(meta, &specialists, Some("paper.tex"));
        let target = &enriched["revision_targets"][0];
        assert_eq!(target["target_kind"], "paper_tex");
        assert_eq!(target["source_path"], "paper.tex");
        assert_eq!(target["locator"], "eq:md");
        assert_eq!(
            target["required_update"],
            "Correct the drawdown formula and verify affected table values."
        );
    }

    #[test]
    fn citation_missing_reference_becomes_bibliography_target() {
        let meta = json!({
            "summary": "Needs work.",
            "strengths": [],
            "weaknesses": ["Haarnoja et al. (2018) is absent from the bibliography."],
            "questions": [],
            "recommendation": "major_revision",
            "confidence": 0.9
        });
        let specialists = json!({
            "specialists": {
                "citation": {
                    "missing_references": [{
                        "title": "Soft Actor-Critic: Off-Policy Maximum Entropy Deep Reinforcement Learning with a Stochastic Actor",
                        "reason": "SAC is the core method."
                    }]
                }
            }
        });
        let enriched = enrich_meta_review(meta, &specialists, None);
        let target = &enriched["revision_targets"][0];
        assert_eq!(target["target_kind"], "bibliography");
        assert_eq!(target["source_role"], "citation");
    }

    #[test]
    fn fallback_inference_targets_common_major_revision_sections() {
        let meta = json!({
            "summary": "Needs work.",
            "strengths": [],
            "weaknesses": [
                "Abnormal returns claims are overstated without any multiple-testing correction.",
                "The EWJ ETF benchmark tracks MSCI Japan in USD, not Nikkei 225 in JPY.",
                "The entropy coefficient is fixed without SAC hyperparameter ablation.",
                "Novelty is incremental and needs clearer contribution framing."
            ],
            "questions": [],
            "recommendation": "major_revision",
            "confidence": 0.9
        });
        let enriched = enrich_meta_review(meta, &json!({"specialists": {}}), Some("paper.tex"));
        let targets = enriched["revision_targets"].as_array().unwrap();

        assert_eq!(targets[0]["target_kind"], "paper_tex");
        assert_eq!(targets[0]["locator"], "multiple-testing correction");
        assert_eq!(targets[1]["target_kind"], "data");
        assert_eq!(targets[1]["locator"], "benchmark/data section");
        assert_eq!(targets[2]["target_kind"], "code");
        assert_eq!(
            targets[2]["locator"],
            "SAC hyperparameters and reward scaling"
        );
        assert_eq!(targets[3]["target_kind"], "paper_tex");
        assert_eq!(targets[3]["locator"], "contribution and novelty framing");
    }

    #[test]
    fn markdown_formats_targets_like_review_findings() {
        let meta = json!({
            "revision_targets": [{
                "id": "weakness-1",
                "weakness_index": 0,
                "source_role": "reproducibility",
                "target_kind": "data",
                "source_path": null,
                "locator": "data availability and restricted inputs",
                "evidence": "Bloomberg membership data are subscription-restricted and no frozen dataset is provided.",
                "required_update": "Add a frozen data snapshot or a reproducible data-access appendix.",
                "verification_check": "Re-review should find data artifacts or access instructions.",
                "status": "open"
            }]
        });
        let markdown = revision_targets_markdown(Some(&meta));

        assert!(markdown.contains("- [ ] **Data availability and restricted inputs**"));
        assert!(markdown.contains("  - Location: data/reproducibility artifacts"));
        assert!(markdown.contains("  - Evidence: Bloomberg membership data"));
        assert!(markdown.contains("  - Required change: Add a frozen data snapshot"));
        assert!(!markdown.contains("[open] data at data:"));
    }

    #[test]
    fn dependency_graph_links_upstream_artifacts_to_manuscript_targets() {
        let meta = json!({
            "revision_targets": [
                {
                    "id": "weakness-1",
                    "weakness_index": 0,
                    "source_role": "technical_correctness",
                    "target_kind": "paper_tex",
                    "source_path": "paper.tex",
                    "locator": "Section 5.2",
                    "evidence": "The paper makes a quantitative speedup claim with no benchmark.",
                    "required_update": "Revise the quantitative speedup claim after adding a reproducible benchmark model.",
                    "verification_check": "Re-review should confirm the manuscript claim matches the benchmark model.",
                    "status": "open"
                },
                {
                    "id": "weakness-2",
                    "weakness_index": 1,
                    "source_role": "reproducibility",
                    "target_kind": "code",
                    "source_path": null,
                    "locator": "code release and execution entrypoints",
                    "evidence": "No runnable source code is provided.",
                    "required_update": "Release source code and entrypoints for the benchmark model.",
                    "verification_check": "Re-review should confirm runnable source code is present.",
                    "status": "open"
                }
            ]
        });
        let graph = revision_dependency_graph_markdown(Some(&meta));

        assert!(graph.contains("```mermaid"));
        assert!(graph.contains("nweakness_2 --> nweakness_1"));
        assert!(graph.contains("`weakness-1` should follow `weakness-2`."));
    }

    #[test]
    fn mermaid_labels_use_entity_codes_not_backslash_escapes() {
        // Regression: GitHub Mermaid rejected backslash-escaped quotes inside `id["…"]`
        // (e.g. `(Construction 55 \"symmofhopf\")`) → "Parse error … got 'STR'". The label
        // must carry NO raw `"`/`(`/`)` and NO backslash escapes.
        let escaped = escape_mermaid_label("Section 6 (Construction 55 \"symmofhopf\")");
        assert_eq!(
            escaped,
            "Section 6 #40;Construction 55 #quot;symmofhopf#quot;#41;"
        );
        assert!(!escaped.contains('"'));
        assert!(!escaped.contains('('));
        assert!(!escaped.contains(')'));
        assert!(!escaped.contains('\\'));
        assert!(escape_mermaid_label("a\nb").contains("a b"));
    }

    #[test]
    fn dependency_graph_renders_labels_with_quotes_and_parens() {
        let meta = json!({
            "revision_targets": [
                {
                    "id": "weakness-1",
                    "weakness_index": 0,
                    "source_role": "technical_correctness",
                    "target_kind": "paper_tex",
                    "source_path": "paper.tex",
                    "locator": "Abstract; Section 6 (Construction 55 \"symmofhopf\")",
                    "evidence": "The categorical Hopf map construction is asserted without proof.",
                    "required_update": "Justify Construction 55 (\"symmofhopf\") with a citation or proof.",
                    "verification_check": "Re-review should confirm the construction is justified.",
                    "status": "open"
                },
                {
                    "id": "weakness-2",
                    "weakness_index": 1,
                    "source_role": "reproducibility",
                    "target_kind": "code",
                    "source_path": null,
                    "locator": "code release and execution entrypoints",
                    "evidence": "No runnable source code is provided.",
                    "required_update": "Release source code and entrypoints.",
                    "verification_check": "Re-review should confirm runnable source code is present.",
                    "status": "open"
                }
            ]
        });
        let graph = revision_dependency_graph_markdown(Some(&meta));

        // The whole mermaid fenced block must contain NO backslash-escaped quote and NO raw
        // quote/paren inside a node label (only the wrapping `["` / `"]` quotes are allowed).
        assert!(graph.contains("```mermaid"));
        assert!(
            !graph.contains("\\\""),
            "backslash-escaped quote breaks GitHub mermaid:\n{graph}"
        );
        assert!(
            graph.contains("#quot;"),
            "expected entity-encoded quote:\n{graph}"
        );
        for line in graph
            .lines()
            .filter(|l| l.trim_start().starts_with("nweakness_") && l.contains("[\""))
        {
            let inner = line
                .split_once("[\"")
                .and_then(|(_, rest)| rest.rsplit_once("\"]"))
                .map(|(label, _)| label)
                .unwrap_or("");
            assert!(!inner.contains('"'), "raw quote in label: {line}");
            assert!(!inner.contains('('), "raw paren in label: {line}");
            assert!(!inner.contains(')'), "raw paren in label: {line}");
        }
    }

    #[test]
    fn prior_targets_are_reconciled_against_new_targets() {
        let prior = json!({
            "revision_targets": [
                {
                    "id": "weakness-1",
                    "weakness_index": 0,
                    "source_role": "technical_correctness",
                    "target_kind": "paper_tex",
                    "source_path": "paper.tex",
                    "locator": "eq:md",
                    "evidence": "bad formula",
                    "required_update": "Correct the maximum drawdown formula.",
                    "verification_check": "Check the formula.",
                    "status": "open"
                },
                {
                    "id": "weakness-2",
                    "weakness_index": 1,
                    "source_role": "citation",
                    "target_kind": "bibliography",
                    "source_path": null,
                    "locator": "Haarnoja 2018",
                    "evidence": "missing",
                    "required_update": "Add Haarnoja 2018.",
                    "verification_check": "Check bibliography.",
                    "status": "open"
                }
            ]
        });
        let next = json!({
            "summary": "Still needs work.",
            "revision_targets": [{
                "id": "weakness-1",
                "weakness_index": 0,
                "source_role": "technical_correctness",
                "target_kind": "paper_tex",
                "source_path": "paper.tex",
                "locator": "eq:md",
                "evidence": "formula still bad",
                "required_update": "Correct the maximum drawdown formula.",
                "verification_check": "Check the formula.",
                "status": "open"
            }]
        });
        let reconciled = reconcile_revision_targets(Some(&prior), next);
        assert_eq!(reconciled["revision_targets"][0]["status"], "still_open");
        assert_eq!(reconciled["revision_targets"][1]["status"], "addressed");
    }
}
