//! GrokRxiv verifier ladder.
//!
//! A [`Verifier`] is a single rung in the ladder that inspects an artifact
//! (JSON) and returns a structured [`VerifierResult`]. The standard ladder
//! runs the four implementations: JSON schema, citation existence, tone, and
//! render integrity.

#![forbid(unsafe_code)]

pub mod citation;
pub mod json_schema;
pub mod metadata;
pub mod render;
pub mod support;
pub mod tone;

use async_trait::async_trait;
use grokrxiv_schemas::{PaperExtract, VerifierResult};

pub use citation::CitationVerifier;
pub use json_schema::JsonSchemaVerifier;
pub use metadata::MetadataVerifier;
pub use render::RenderVerifier;
pub use support::SupportVerifier;
pub use tone::ToneVerifier;

/// Context every verifier receives at run time.
pub struct VerifierContext<'a> {
    /// The paper currently being reviewed; verifiers can use it for citation
    /// cross-checks and similar.
    pub paper: &'a PaperExtract,
    /// Shared HTTP client (reqwest) so verifiers don't each spin up their own.
    pub http: &'a reqwest::Client,
}

/// A single rung in the verifier ladder. Implementations are stateless aside
/// from caches.
#[async_trait]
pub trait Verifier: Send + Sync {
    /// Stable identifier (snake_case) used in logs and the database.
    fn name(&self) -> &'static str;
    /// Inspect the artifact and return a structured outcome.
    async fn verify(
        &self,
        artifact: &serde_json::Value,
        ctx: &VerifierContext<'_>,
    ) -> VerifierResult;
}

/// Ordered ladder of verifier rungs.
pub struct VerifierLadder {
    steps: Vec<Box<dyn Verifier>>,
}

impl VerifierLadder {
    /// Construct an empty ladder.
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Construct the standard ladder: schema → metadata → support → citation →
    /// tone → render.
    ///
    /// `schema` defaults to a permissive object schema if no caller-specific
    /// schema is provided; pass `Some(schema_json)` to validate against the
    /// agent's contract.
    pub fn standard(schema: Option<serde_json::Value>) -> Self {
        let mut l = Self::new();
        l.steps.push(Box::new(JsonSchemaVerifier::new(
            schema.unwrap_or_else(|| serde_json::json!({ "type": "object" })),
        )));
        l.steps.push(Box::new(MetadataVerifier::new()));
        l.steps.push(Box::new(SupportVerifier::new()));
        l.steps.push(Box::new(CitationVerifier::new()));
        l.steps.push(Box::new(ToneVerifier::new()));
        l.steps.push(Box::new(RenderVerifier::new()));
        l
    }

    /// Append a custom rung.
    pub fn push(&mut self, v: Box<dyn Verifier>) {
        self.steps.push(v);
    }

    /// Run every rung against the artifact, in order. Returns a `(name, result)`
    /// tuple per rung so callers can persist them all.
    pub async fn run(
        &self,
        artifact: &serde_json::Value,
        ctx: &VerifierContext<'_>,
    ) -> Vec<(String, VerifierResult)> {
        let mut out = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            let result = step.verify(artifact, ctx).await;
            out.push((step.name().to_string(), result));
        }
        out
    }
}

impl Default for VerifierLadder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::PaperExtract;
    use serde_json::json;

    fn paper() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2605.00001".to_string(),
            title: "Verifier Paper".to_string(),
            authors: Vec::new(),
            abstract_: "A paper abstract.".to_string(),
            field: Some("cs.AI".to_string()),
            sections: Vec::new(),
            figures: Vec::new(),
            bibliography: Vec::new(),
        }
    }

    #[tokio::test]
    async fn standard_ladder_includes_metadata_and_support_rungs() {
        let ladder = VerifierLadder::standard(Some(json!({ "type": "object" })));
        let http = reqwest::Client::new();
        let paper = paper();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };

        let names: Vec<String> = ladder
            .run(
                &json!({ "summary": "supported review", "strengths": ["clear"] }),
                &ctx,
            )
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(names.contains(&"metadata".to_string()));
        assert!(names.contains(&"support".to_string()));
    }
}
