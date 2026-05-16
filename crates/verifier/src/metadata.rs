//! Metadata consistency verifier.
//!
//! This rung checks that the paper context attached to an agent artifact is
//! usable for public provenance: a non-empty arXiv id, title, and abstract, plus
//! a syntactically plausible modern arXiv identifier. It does not make network
//! requests; live arXiv lookups belong in the ingest layer so they stay behind
//! the rate gate.

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
        let paper = ctx.paper;
        let mut missing = Vec::new();
        if paper.arxiv_id.trim().is_empty() {
            missing.push("arxiv_id");
        }
        if paper.title.trim().is_empty() {
            missing.push("title");
        }
        if paper.abstract_.trim().is_empty() {
            missing.push("abstract");
        }
        let arxiv_shape_ok = arxiv_id_shape_ok(&paper.arxiv_id);
        let status = if missing.is_empty() && arxiv_shape_ok {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Warn
        };
        VerifierResult {
            status,
            notes: json!({
                "missing": missing,
                "arxiv_shape_ok": arxiv_shape_ok,
                "field_present": paper.field.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()),
            }),
        }
    }
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
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let result = MetadataVerifier::new()
            .verify(&serde_json::json!({}), &ctx)
            .await;
        assert_eq!(result.status, VerifierStatus::Pass);
    }
}
