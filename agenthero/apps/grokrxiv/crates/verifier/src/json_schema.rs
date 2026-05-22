//! JSON-schema verifier rung.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use serde_json::json;

use crate::{Verifier, VerifierContext};

/// Validates the artifact against a JSON Schema. Fails on invalid input.
pub struct JsonSchemaVerifier {
    schema_json: serde_json::Value,
}

impl JsonSchemaVerifier {
    /// Construct a verifier with the supplied schema document.
    pub fn new(schema_json: serde_json::Value) -> Self {
        Self { schema_json }
    }
}

#[async_trait]
impl Verifier for JsonSchemaVerifier {
    fn name(&self) -> &'static str {
        "json_schema"
    }

    async fn verify(
        &self,
        artifact: &serde_json::Value,
        _ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        let validator = match jsonschema::validator_for(&self.schema_json) {
            Ok(v) => v,
            Err(e) => {
                return VerifierResult {
                    status: VerifierStatus::Fail,
                    notes: json!({ "error": format!("invalid schema: {e}") }),
                };
            }
        };
        let errors: Vec<String> = validator
            .iter_errors(artifact)
            .map(|e| e.to_string())
            .collect();
        if errors.is_empty() {
            VerifierResult {
                status: VerifierStatus::Pass,
                notes: json!({}),
            }
        } else {
            VerifierResult {
                status: VerifierStatus::Fail,
                notes: json!({ "errors": errors }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::PaperExtract;

    fn ctx_paper() -> PaperExtract {
        PaperExtract {
            arxiv_id: "x".into(),
            title: "t".into(),
            authors: vec![],
            abstract_: "a".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    #[tokio::test]
    async fn passes_valid_artifact() {
        let v = JsonSchemaVerifier::new(json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        }));
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({ "foo": "bar" }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass));
    }

    #[tokio::test]
    async fn fails_invalid_artifact() {
        let v = JsonSchemaVerifier::new(json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        }));
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v.verify(&json!({ "foo": 42 }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Fail));
        assert!(r.notes.get("errors").is_some());
    }
}
