//! Metadata consistency verifier.
//!
//! This rung checks that the subject context attached to an agent artifact is
//! usable for provenance: a non-empty source id, title, and summary/abstract.
//! Paper subjects additionally get arXiv or synthetic local/git source-shape
//! checks. It does not make network requests; live arXiv lookups belong in the
//! ingest layer so they stay behind the rate gate.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use serde_json::json;

use crate::{Verifier, VerifierContext};

/// Verifies local paper metadata needed for review provenance.
#[derive(Default)]
pub struct MetadataVerifier;

impl MetadataVerifier {
    /// Construct a metadata verifier.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Verifier for MetadataVerifier {
    fn name(&self) -> &'static str {
        "metadata"
    }

    async fn verify(
        &self,
        _artifact: &serde_json::Value,
        ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        let mut missing = Vec::new();
        let id = ctx
            .subject_str("id")
            .or_else(|| ctx.subject_str("arxiv_id"))
            .unwrap_or("");
        let title = ctx.subject_str("title").unwrap_or("");
        let summary = ctx
            .subject_str("summary")
            .or_else(|| ctx.subject_str("abstract"))
            .unwrap_or("");

        if id.trim().is_empty() {
            missing.push(if ctx.is_paper() { "arxiv_id" } else { "id" });
        }
        if title.trim().is_empty() {
            missing.push("title");
        }
        if summary.trim().is_empty() {
            missing.push(if ctx.is_paper() {
                "abstract"
            } else {
                "summary"
            });
        }
        let arxiv_shape_ok = ctx.is_paper() && arxiv_id_shape_ok(id);
        let source_id_kind = if ctx.is_paper() {
            source_id_kind(id)
        } else if id.trim().is_empty() {
            None
        } else {
            Some("generic")
        };
        let source_id_shape_ok = source_id_kind.is_some();
        let status = if missing.is_empty() && source_id_shape_ok {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Warn
        };
        VerifierResult {
            status,
            notes: json!({
                "missing": missing,
                "source_id_shape_ok": source_id_shape_ok,
                "source_id_kind": source_id_kind,
                "arxiv_shape_ok": arxiv_shape_ok,
                "field_present": ctx.subject_str("field").map(str::trim).is_some_and(|s| !s.is_empty()),
                "subject_kind": ctx.subject_kind.as_str(),
            }),
        }
    }
}

fn source_id_kind(id: &str) -> Option<&'static str> {
    if arxiv_id_shape_ok(id) {
        return Some("arxiv");
    }
    if synthetic_source_id_shape_ok(id) {
        return id
            .strip_prefix("local-pdf-")
            .map(|_| "local_pdf")
            .or_else(|| id.strip_prefix("local-tex-").map(|_| "local_tex"))
            .or_else(|| id.strip_prefix("git-tex-").map(|_| "git_tex"))
            .or_else(|| id.strip_prefix("git-repo-").map(|_| "git_repo"));
    }
    None
}

fn synthetic_source_id_shape_ok(id: &str) -> bool {
    const PREFIXES: [&str; 4] = ["local-pdf-", "local-tex-", "git-tex-", "git-repo-"];
    let Some(rest) = PREFIXES.iter().find_map(|prefix| id.strip_prefix(prefix)) else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn arxiv_id_shape_ok(id: &str) -> bool {
    let core = id.strip_suffix_version();
    if core.contains('/') {
        let Some((subject, number)) = core.split_once('/') else {
            return false;
        };
        return !subject.is_empty()
            && number.len() == 7
            && number.chars().all(|c| c.is_ascii_digit());
    }
    let Some((yymm, number)) = core.split_once('.') else {
        return false;
    };
    yymm.len() == 4
        && yymm.chars().all(|c| c.is_ascii_digit())
        && (number.len() == 4 || number.len() == 5 || number.len() == 6)
        && number.chars().all(|c| c.is_ascii_digit())
}

trait ArxivVersionSuffix {
    fn strip_suffix_version(&self) -> &str;
}

impl ArxivVersionSuffix for str {
    fn strip_suffix_version(&self) -> &str {
        match self.rfind('v') {
            Some(idx) if idx > 0 && self[idx + 1..].chars().all(|c| c.is_ascii_digit()) => {
                &self[..idx]
            }
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::PaperExtract;

    fn ctx(arxiv_id: &str) -> (PaperExtract, reqwest::Client) {
        (
            PaperExtract {
                arxiv_id: arxiv_id.to_string(),
                title: "Title".to_string(),
                authors: Vec::new(),
                abstract_: "Abstract".to_string(),
                field: Some("cs.AI".to_string()),
                sections: Vec::new(),
                figures: Vec::new(),
                bibliography: Vec::new(),
                source_format: None,
            },
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn passes_modern_arxiv_id_with_metadata() {
        let (paper, http) = ctx("2605.12345v2");
        let ctx = VerifierContext::for_paper(&paper, &http);
        let result = MetadataVerifier::new()
            .verify(&serde_json::json!({}), &ctx)
            .await;
        assert_eq!(result.status, VerifierStatus::Pass);
    }

    #[tokio::test]
    async fn passes_local_and_git_source_ids_with_metadata() {
        for source_id in [
            "local-pdf-a1b2c3d4",
            "local-tex-deadbeef",
            "git-tex-abc123def456",
            "git-repo-feature_source-review-abstraction",
        ] {
            let (paper, http) = ctx(source_id);
            let ctx = VerifierContext::for_paper(&paper, &http);
            let result = MetadataVerifier::new()
                .verify(&serde_json::json!({}), &ctx)
                .await;

            assert_eq!(
                result.status,
                VerifierStatus::Pass,
                "{source_id} should pass metadata"
            );
            assert_eq!(result.notes["source_id_shape_ok"], true);
            assert_eq!(result.notes["arxiv_shape_ok"], false);
        }
    }

    #[tokio::test]
    async fn warns_on_unknown_source_id_shape() {
        let (paper, http) = ctx("not-a-valid-source-id");
        let ctx = VerifierContext::for_paper(&paper, &http);
        let result = MetadataVerifier::new()
            .verify(&serde_json::json!({}), &ctx)
            .await;

        assert_eq!(result.status, VerifierStatus::Warn);
        assert_eq!(result.notes["source_id_shape_ok"], false);
    }
}
