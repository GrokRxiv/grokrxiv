//! Generic Lean artifact policy checks for GrokRxiv-generated code.
//!
//! This module intentionally avoids paper-specific mathematical heuristics.
//! It checks mechanical contracts the platform owns, then leaves semantic
//! source-faithfulness to Lean compilation and the LLM reviewer stages.

use serde::Serialize;
use std::collections::BTreeSet;

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
}
