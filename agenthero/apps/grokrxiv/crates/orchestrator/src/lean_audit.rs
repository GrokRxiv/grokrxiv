//! Generic Lean artifact policy checks for GrokRxiv-generated code.
//!
//! This module intentionally avoids paper-specific mathematical heuristics.
//! It checks mechanical contracts the platform owns, then leaves semantic
//! source-faithfulness to Lean compilation and the LLM reviewer stages.

use serde::Serialize;
use std::collections::BTreeSet;
use uuid::Uuid;

/// A data-driven policy for generated Lean artifacts.
#[derive(Debug, Clone)]
pub(crate) struct LeanArtifactPolicy<'a> {
    /// Files that must be present in the artifact.
    pub(crate) required_files: &'a [&'a str],
    /// Lean tokens that are forbidden by the current role contract.
    pub(crate) forbidden_terms: &'a [&'a str],
    /// File paths where `opaque` declarations may appear.
    pub(crate) opaque_allowed_paths: &'a [&'a str],
    /// Whether any use of `opaque` requires source evidence in the manifest.
    pub(crate) require_source_evidence_for_opaque: bool,
}

/// A structured policy issue from a Lean artifact audit.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LeanAuditIssue {
    /// Stable machine-readable issue kind.
    pub(crate) kind: String,
    /// File path when the issue is file-scoped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Forbidden token when the issue is token-scoped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) term: Option<String>,
    /// Human-readable detail for logs and fixer prompts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

impl LeanAuditIssue {
    fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            path: None,
            term: None,
            detail: None,
        }
    }

    fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn term(mut self, term: impl Into<String>) -> Self {
        self.term = Some(term.into());
        self
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Audit a paper-local Lean library artifact against generic platform policy.
pub(crate) fn audit_lean_library_artifact(
    artifact: &serde_json::Value,
    policy: &LeanArtifactPolicy<'_>,
) -> Vec<LeanAuditIssue> {
    let mut issues = Vec::new();
    let files = artifact
        .get("files")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let file_codes = files
        .iter()
        .map(|file| {
            (
                file.get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<missing path>")
                    .to_string(),
                file.get("code")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    let file_paths = files
        .iter()
        .filter_map(|file| file.get("path").and_then(|value| value.as_str()))
        .collect::<BTreeSet<_>>();

    for required in policy.required_files {
        if !file_paths.contains(required) {
            issues.push(
                LeanAuditIssue::new("missing_required_file")
                    .path(*required)
                    .detail(format!(
                        "required Lean artifact file `{required}` is missing"
                    )),
            );
        }
    }

    let opaque_allowed = policy
        .opaque_allowed_paths
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut saw_opaque = false;
    for (path, code) in &file_codes {
        for term in forbidden_terms_in_code(code, policy.forbidden_terms) {
            issues.push(
                LeanAuditIssue::new("forbidden_term")
                    .path(path)
                    .term(term)
                    .detail(format!("forbidden Lean term `{term}` appears in `{path}`")),
            );
        }
        if !forbidden_terms_in_code(code, &["opaque"]).is_empty() {
            saw_opaque = true;
            if !opaque_allowed.contains(path.as_str()) {
                issues.push(
                    LeanAuditIssue::new("opaque_outside_allowed_path")
                        .path(path)
                        .detail(format!(
                            "`opaque` is not allowed in `{path}` by this policy"
                        )),
                );
            }
        }
    }

    if saw_opaque
        && policy.require_source_evidence_for_opaque
        && !manifest_has_source_backed_interface(artifact.get("manifest"))
    {
        issues.push(
            LeanAuditIssue::new("opaque_missing_source_evidence").detail(
                "`opaque` appears in generated Lean, but the manifest has no source-backed interface declaration",
            ),
        );
    }

    issues
}

/// Build the source-first Lean authoring input for one theorem_inventory claim.
///
/// This deliberately preserves source TeX and nearby source context while
/// excluding typed/semantic IR fields. Lean generation is authored by the LLM
/// from paper source evidence, not gated by an intermediate math IR.
pub(crate) fn build_lean_source_input_for_claim(
    review_id: Uuid,
    inventory: &serde_json::Value,
    claim_id: &str,
) -> anyhow::Result<serde_json::Value> {
    let target = inventory
        .get("items")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter(|item| item.get("role").and_then(|value| value.as_str()) == Some("lean_target"))
        .find(|item| inventory_item_matches_claim_id(item, claim_id))
        .cloned()
        .map(strip_ir_fields)
        .ok_or_else(|| anyhow::anyhow!("inventory lean target `{claim_id}` not found"))?;

    Ok(serde_json::json!({
        "schema_version": "1.0.0",
        "review_id": review_id,
        "source": "review_loop/theorem_inventory.json",
        "pipeline": "theorem_inventory_direct",
        "target": target,
        "contract": {
            "source_tex_is_authoritative": true,
            "source_context_is_supporting_evidence": true,
            "llm_authors_statement_and_proof": true,
            "deterministic_math_generation_allowed": false,
            "forbidden_replacements": ["True", "0 = 0", "x = x", "metadata-only claims"],
            "forbidden_terms": ["sorry", "admit", "axiom"]
        }
    }))
}

/// Extract a source-first theorem inventory already embedded in
/// paper_math_sources, dropping intermediate typed/semantic IR fields.
pub(crate) fn source_first_inventory_from_paper_math_sources(
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    value
        .pointer("/theorem_graph/source_inventory")
        .or_else(|| value.get("theorem_inventory"))
        .cloned()
        .map(strip_ir_fields)
}

/// Whether a target/library Lean result has concrete diagnostics that should be
/// handed to a Lean fixer. Runner/environment failures are intentionally false.
pub(crate) fn lean_compile_result_is_fixable(compile_result: &serde_json::Value) -> bool {
    matches!(
        compile_result
            .get("status")
            .and_then(|value| value.as_str()),
        Some(
            "lean_compile_error"
                | "forbidden_term"
                | "missing_lean_declaration"
                | "missing_paper_library_import"
        )
    )
}

/// Whether a failed target compile should trigger paper-local library feedback.
pub(crate) fn lean_target_compile_needs_library_feedback(
    compile_result: &serde_json::Value,
) -> bool {
    compile_result
        .get("status")
        .and_then(|value| value.as_str())
        == Some("lean_compile_error")
}

/// Whether a failed paper-local library compile should trigger a library fix.
pub(crate) fn lean_library_compile_needs_fix(library_compile: &serde_json::Value) -> bool {
    matches!(
        library_compile
            .get("status")
            .and_then(|value| value.as_str()),
        Some("lean_compile_error" | "validation_error")
    )
}

/// Build the durable diagnostic artifact for a Lean/Lake target check.
pub(crate) fn lean_check_diagnostic_result(
    claim_id: &str,
    target_slug: &str,
    status: &str,
    formal_claim_present: bool,
    paper_library_imports_present: bool,
    forbidden_terms: &[&str],
    compile_report: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": "1.0.0",
        "stage": "lean-check",
        "diagnostic_only": true,
        "status": status,
        "claim_id": claim_id,
        "formal_claim_present": formal_claim_present,
        "paper_library_imports_present": paper_library_imports_present,
        "forbidden_terms": forbidden_terms,
        "compile": compile_report,
        "code_path": format!("review_loop/lean/targets/{target_slug}/check/GrokRxiv/Proofs.lean"),
        "proofs_lean": format!("review_loop/lean/targets/{target_slug}/GrokRxiv/Proofs.lean"),
        "proofs_lean_saved": true,
        "note": "Lean/Lake compile is a diagnostic for this MVP; GrokRxiv/Proofs.lean is preserved even when compile fails because paper-local or Mathlib support may be incomplete.",
    })
}

fn inventory_item_matches_claim_id(item: &serde_json::Value, claim_id: &str) -> bool {
    item.get("id")
        .and_then(|value| value.as_str())
        .is_some_and(|id| id == claim_id)
        || item
            .get("labels")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .any(|label| label == claim_id)
}

fn strip_ir_fields(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(map) => {
            for key in [
                "typed_ir",
                "typed_transcription",
                "theorem_ir",
                "semantic_ir",
                "typed_ir_quality_issue",
            ] {
                map.remove(key);
            }
            for child in map.values_mut() {
                let stripped = strip_ir_fields(std::mem::take(child));
                *child = stripped;
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                let stripped = strip_ir_fields(std::mem::take(child));
                *child = stripped;
            }
        }
        _ => {}
    }
    value
}

fn manifest_has_source_backed_interface(manifest: Option<&serde_json::Value>) -> bool {
    manifest
        .and_then(|value| value.get("declarations"))
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .any(|declaration| {
            declaration.get("kind").and_then(|value| value.as_str()) == Some("interface")
                && declaration
                    .get("source_evidence")
                    .and_then(|value| value.as_array())
                    .is_some_and(|items| !items.is_empty())
        })
}

/// Find forbidden Lean tokens while ignoring comments, strings, and identifier substrings.
pub(crate) fn forbidden_terms_in_code<'a>(code: &str, terms: &'a [&'a str]) -> Vec<&'a str> {
    let searchable = lean_code_without_comments_or_strings(code);
    terms
        .iter()
        .copied()
        .filter(|term| lean_code_contains_token(&searchable, term))
        .collect()
}

fn lean_code_contains_token(code: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(offset) = code[start..].find(needle) {
        let idx = start + offset;
        let end = idx + needle.len();
        if !is_lean_ident_continue_before(code, idx) && !is_lean_ident_continue_after(code, end) {
            return true;
        }
        start = end;
    }
    false
}

fn is_lean_ident_continue_before(code: &str, idx: usize) -> bool {
    code[..idx]
        .chars()
        .next_back()
        .map(is_lean_ident_continue)
        .unwrap_or(false)
}

fn is_lean_ident_continue_after(code: &str, idx: usize) -> bool {
    code[idx..]
        .chars()
        .next()
        .map(is_lean_ident_continue)
        .unwrap_or(false)
}

fn is_lean_ident_continue(ch: char) -> bool {
    ch == '_' || ch == '\'' || ch.is_alphanumeric()
}

fn lean_code_without_comments_or_strings(code: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment(usize),
        String,
    }

    let mut out = String::with_capacity(code.len());
    let mut chars = code.char_indices().peekable();
    let mut state = State::Code;
    let mut escaped = false;

    while let Some((_, ch)) = chars.next() {
        match state {
            State::Code => match ch {
                '-' if chars.peek().map(|(_, next)| *next) == Some('-') => {
                    out.push(' ');
                    if let Some((_, next)) = chars.next() {
                        out.push(if next == '\n' { '\n' } else { ' ' });
                        if next == '\n' {
                            state = State::Code;
                        } else {
                            state = State::LineComment;
                        }
                    }
                }
                '/' if chars.peek().map(|(_, next)| *next) == Some('-') => {
                    out.push(' ');
                    let _ = chars.next();
                    out.push(' ');
                    state = State::BlockComment(1);
                }
                '"' => {
                    out.push(' ');
                    escaped = false;
                    state = State::String;
                }
                _ => out.push(ch),
            },
            State::LineComment => {
                if ch == '\n' {
                    out.push('\n');
                    state = State::Code;
                } else {
                    out.push(' ');
                }
            }
            State::BlockComment(depth) => {
                if ch == '/' && chars.peek().map(|(_, next)| *next) == Some('-') {
                    out.push(' ');
                    let _ = chars.next();
                    out.push(' ');
                    state = State::BlockComment(depth + 1);
                } else if ch == '-' && chars.peek().map(|(_, next)| *next) == Some('/') {
                    out.push(' ');
                    let _ = chars.next();
                    out.push(' ');
                    state = if depth == 1 {
                        State::Code
                    } else {
                        State::BlockComment(depth - 1)
                    };
                } else if ch == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            State::String => {
                if ch == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    state = State::Code;
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_terms_ignore_comments_strings_and_identifier_substrings() {
        let terms = ["sorry", "admit", "axiom"];
        let honest_commentary = r#"
import Mathlib

/- This file says axiomatizing the unavailable source objects would be invalid.
   It also mentions sorry/admit in a comment and "sorry" in a string below. -/
def explanation : String := "do not use sorry, admit, or axiom"
theorem grokrxiv_add_zero (n : Nat) : n + 0 = n := by
  have h' : n + 0 = n := by simp
  exact h'
"#;
        assert!(forbidden_terms_in_code(honest_commentary, &terms).is_empty());
        assert_eq!(
            forbidden_terms_in_code("theorem bad : True := by\n  sorry\n", &terms),
            vec!["sorry"]
        );
        assert_eq!(
            forbidden_terms_in_code("axiom source_prop : Prop\n", &terms),
            vec!["axiom"]
        );
        assert_eq!(
            forbidden_terms_in_code("theorem bad : True := by\n  admit\n", &terms),
            vec!["admit"]
        );
        assert!(forbidden_terms_in_code("def not_sorryful : Nat := 0\n", &terms).is_empty());
    }

    #[test]
    fn library_audit_applies_configured_mechanical_policy() {
        let artifact = serde_json::json!({
            "language": "lean",
            "files": [
                {
                    "path": "GrokRxiv/Paper/Definitions.lean",
                    "code": "import Mathlib\n\nopaque LocalObject : Type\n"
                },
                {
                    "path": "GrokRxiv/Paper/Interfaces.lean",
                    "code": "import Mathlib\n\nopaque SourceInterface : Type\n"
                }
            ],
            "manifest": {
                "schema_version": "1.0.0",
                "declarations": []
            },
            "notes": [],
            "confidence": 0.5
        });
        let policy = LeanArtifactPolicy {
            required_files: &[
                "GrokRxiv/Paper/Definitions.lean",
                "GrokRxiv/Paper/Interfaces.lean",
                "GrokRxiv/Paper/Statements.lean",
            ],
            forbidden_terms: &["sorry", "admit", "axiom"],
            opaque_allowed_paths: &["GrokRxiv/Paper/Interfaces.lean"],
            require_source_evidence_for_opaque: true,
        };

        let issues = audit_lean_library_artifact(&artifact, &policy);

        assert!(issues
            .iter()
            .any(|issue| issue.kind == "missing_required_file"));
        assert!(issues
            .iter()
            .any(|issue| issue.kind == "opaque_outside_allowed_path"));
        assert!(issues
            .iter()
            .any(|issue| issue.kind == "opaque_missing_source_evidence"));
    }

    #[test]
    fn lean_source_input_preserves_source_context_and_removes_ir_fields() {
        let review_id = Uuid::parse_str("59486169-9357-42b4-b520-339723816013").unwrap();
        let inventory = serde_json::json!({
            "items": [{
                "id": "prop:ctx",
                "kind": "proposition",
                "role": "lean_target",
                "labels": ["prop:ctx"],
                "source_tex": "\\begin{proposition}The map is an isomorphism.\\end{proposition}",
                "source_context": {
                    "before": "Definitions before.",
                    "after": "Explanation after.",
                    "typed_transcription": {"status": "stale"}
                },
                "typed_transcription": {"status": "transcribed"},
                "theorem_ir": {"conclusion": {"kind": "equality"}},
                "semantic_ir": {"kind": "old_semantics"},
                "typed_ir_quality_issue": "stale"
            }]
        });

        let packet =
            build_lean_source_input_for_claim(review_id, &inventory, "prop:ctx").expect("packet");

        assert_eq!(packet["source"], "review_loop/theorem_inventory.json");
        assert_eq!(packet["target"]["id"], "prop:ctx");
        assert_eq!(
            packet["target"]["source_tex"],
            "\\begin{proposition}The map is an isomorphism.\\end{proposition}"
        );
        assert_eq!(
            packet["target"]["source_context"]["before"],
            "Definitions before."
        );
        assert_eq!(packet.pointer("/target/typed_transcription"), None);
        assert_eq!(packet.pointer("/target/theorem_ir"), None);
        assert_eq!(packet.pointer("/target/semantic_ir"), None);
        assert_eq!(
            packet.pointer("/target/source_context/typed_transcription"),
            None
        );
        assert_eq!(packet.pointer("/typed_ir_required"), None);
    }

    #[test]
    fn compile_policy_only_fixes_real_lean_errors() {
        for status in [
            "lean_compile_error",
            "forbidden_term",
            "missing_lean_declaration",
            "missing_paper_library_import",
        ] {
            assert!(
                lean_compile_result_is_fixable(&serde_json::json!({"status": status})),
                "{status} should be routed to Lean repair"
            );
        }
        for status in [
            "pass",
            "agent_error",
            "runner_timeout",
            "environment_error",
            "missing_prerequisite",
        ] {
            assert!(
                !lean_compile_result_is_fixable(&serde_json::json!({"status": status})),
                "{status} should not be routed to Lean repair"
            );
        }
    }

    #[test]
    fn library_feedback_policy_is_status_based_not_paper_specific() {
        assert!(lean_target_compile_needs_library_feedback(
            &serde_json::json!({"status": "lean_compile_error"})
        ));
        assert!(!lean_target_compile_needs_library_feedback(
            &serde_json::json!({"status": "missing_lean_declaration"})
        ));
        assert!(lean_library_compile_needs_fix(
            &serde_json::json!({"status": "lean_compile_error"})
        ));
        assert!(lean_library_compile_needs_fix(
            &serde_json::json!({"status": "validation_error"})
        ));
        assert!(!lean_library_compile_needs_fix(
            &serde_json::json!({"status": "environment_error"})
        ));
    }

    #[test]
    fn lean_check_diagnostic_result_preserves_output_path_on_compile_failure() {
        let result = lean_check_diagnostic_result(
            "lem:sample",
            "lem_sample",
            "lean_compile_error",
            true,
            true,
            &[],
            serde_json::json!({"status": "fail", "stderr": "error: build failed"}),
        );

        assert_eq!(result["stage"], "lean-check");
        assert_eq!(result["diagnostic_only"], true);
        assert_eq!(result["status"], "lean_compile_error");
        assert_eq!(result["proofs_lean_saved"], true);
        assert_eq!(
            result["proofs_lean"],
            "review_loop/lean/targets/lem_sample/GrokRxiv/Proofs.lean"
        );
        assert!(result["note"]
            .as_str()
            .unwrap()
            .contains("compile is a diagnostic"));
    }
}
