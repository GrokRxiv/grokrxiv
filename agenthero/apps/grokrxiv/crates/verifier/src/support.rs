//! Evidence-support verifier.
//!
//! The MVP support rung is intentionally structural: it checks that an artifact
//! is not an empty shell and that object/array values contain at least one
//! substantive string or nested value. Role-specific schemas enforce exact
//! fields; this rung catches vacuous but schema-shaped artifacts.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use serde_json::json;

use crate::{Verifier, VerifierContext};

/// Verifies that an artifact carries substantive support content.
#[derive(Default)]
pub struct SupportVerifier;

impl SupportVerifier {
    /// Construct a support verifier.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Verifier for SupportVerifier {
    fn name(&self) -> &'static str {
        "support"
    }

    async fn verify(
        &self,
        artifact: &serde_json::Value,
        _ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        let substantive_strings = count_substantive_strings(artifact);
        let non_empty_arrays = count_non_empty_arrays(artifact);
        let ok = substantive_strings > 0 || non_empty_arrays > 0;
        VerifierResult {
            status: if ok {
                VerifierStatus::Pass
            } else {
                VerifierStatus::Warn
            },
            notes: json!({
                "substantive_strings": substantive_strings,
                "non_empty_arrays": non_empty_arrays,
            }),
        }
    }
}

fn count_substantive_strings(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(s) => usize::from(s.trim().chars().count() >= 8),
        serde_json::Value::Array(values) => values.iter().map(count_substantive_strings).sum(),
        serde_json::Value::Object(map) => map.values().map(count_substantive_strings).sum(),
        _ => 0,
    }
}

fn count_non_empty_arrays(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => {
            usize::from(!values.is_empty())
                + values.iter().map(count_non_empty_arrays).sum::<usize>()
        }
        serde_json::Value::Object(map) => map.values().map(count_non_empty_arrays).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::PaperExtract;

    fn ctx() -> (PaperExtract, reqwest::Client) {
        (
            PaperExtract {
                arxiv_id: "2605.12345".to_string(),
                title: "Title".to_string(),
                authors: Vec::new(),
                abstract_: "Abstract".to_string(),
                field: None,
                sections: Vec::new(),
                figures: Vec::new(),
                bibliography: Vec::new(),
                source_format: None,
            },
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn warns_on_empty_artifact() {
        let (paper, http) = ctx();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let result = SupportVerifier::new()
            .verify(&serde_json::json!({}), &ctx)
            .await;
        assert_eq!(result.status, VerifierStatus::Warn);
    }
}
