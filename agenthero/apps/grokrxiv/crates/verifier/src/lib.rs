//! GrokRxiv verifier ladder.
//!
//! A [`Verifier`] is a single rung in the ladder that inspects an artifact
//! (JSON) and returns a structured [`VerifierResult`]. The standard ladder
//! runs the implementations for schema, metadata, support, citation existence,
//! tone, and render integrity.

#![forbid(unsafe_code)]

pub mod citation;
pub mod json_schema;
pub mod metadata;
pub mod render;
pub mod support;
pub mod tone;

use async_trait::async_trait;
use grokrxiv_schemas::{Citation, PaperExtract, VerifierResult};

pub use citation::CitationVerifier;
pub use json_schema::JsonSchemaVerifier;
pub use metadata::MetadataVerifier;
pub use render::RenderVerifier;
pub use support::SupportVerifier;
pub use tone::ToneVerifier;

/// Context every verifier receives at run time.
pub struct VerifierContext<'a> {
    /// Artifact kind, e.g. `paper`, `codebase`, or `schema`.
    pub subject_kind: String,
    /// Generic subject metadata. Paper-specific callers convert
    /// [`PaperExtract`] into this JSON form before invoking the verifier.
    pub subject: serde_json::Value,
    /// Shared HTTP client (reqwest) so verifiers don't each spin up their own.
    pub http: &'a reqwest::Client,
}

impl<'a> VerifierContext<'a> {
    /// Construct a verifier context for any DAG artifact subject.
    pub fn new(
        subject_kind: impl Into<String>,
        subject: serde_json::Value,
        http: &'a reqwest::Client,
    ) -> Self {
        Self {
            subject_kind: subject_kind.into(),
            subject,
            http,
        }
    }

    /// Construct a verifier context for a GrokRxiv paper extract.
    pub fn for_paper(paper: &PaperExtract, http: &'a reqwest::Client) -> Self {
        Self::new("paper", paper_subject(paper), http)
    }

    /// Whether this context represents a paper artifact.
    pub fn is_paper(&self) -> bool {
        self.subject_kind == "paper"
            || self
                .subject
                .get("kind")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| kind == "paper")
    }

    /// Read a string field from the generic subject.
    pub fn subject_str(&self, key: &str) -> Option<&str> {
        self.subject.get(key).and_then(|value| value.as_str())
    }

    /// Extract bibliography entries when the subject is a paper.
    pub fn paper_bibliography(&self) -> Option<Vec<Citation>> {
        if !self.is_paper() {
            return None;
        }
        serde_json::from_value(self.subject.get("bibliography")?.clone()).ok()
    }
}

/// Convert a GrokRxiv paper extract into the generic verifier subject shape.
pub fn paper_subject(paper: &PaperExtract) -> serde_json::Value {
    let mut subject = serde_json::to_value(paper).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(map) = subject.as_object_mut() {
        map.insert("kind".to_string(), serde_json::json!("paper"));
        map.insert("id".to_string(), serde_json::json!(paper.arxiv_id.clone()));
        map.entry("summary".to_string())
            .or_insert_with(|| serde_json::json!(paper.abstract_.clone()));
    }
    subject
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
        Self::with_citation(schema, true)
    }

    /// Construct a ladder from YAML verifier names. Citation lookups run only
    /// when the agent config declares a citation verifier.
    pub fn standard_for_config(verifiers: &[String], schema: Option<serde_json::Value>) -> Self {
        Self::with_citation(
            schema,
            verifiers.iter().any(|name| {
                let name = name.to_ascii_lowercase();
                matches!(name.as_str(), "citation" | "citation_existence")
            }),
        )
    }

    fn with_citation(schema: Option<serde_json::Value>, include_citation: bool) -> Self {
        let mut l = Self::new();
        l.steps.push(Box::new(JsonSchemaVerifier::new(
            schema.unwrap_or_else(|| serde_json::json!({ "type": "object" })),
        )));
        l.steps.push(Box::new(MetadataVerifier::new()));
        l.steps.push(Box::new(SupportVerifier::new()));
        if include_citation {
            l.steps.push(Box::new(CitationVerifier::new()));
        }
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
            source_format: None,
        }
    }

    #[tokio::test]
    async fn standard_ladder_includes_metadata_and_support_rungs() {
        let ladder = VerifierLadder::standard(Some(json!({ "type": "object" })));
        let http = reqwest::Client::new();
        let paper = paper();
        let ctx = VerifierContext::for_paper(&paper, &http);

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

    #[tokio::test]
    async fn role_aware_ladder_excludes_citation_for_non_citation_roles() {
        let ladder = VerifierLadder::standard_for_config(
            &["json_schema".to_string()],
            Some(json!({ "type": "object" })),
        );
        let http = reqwest::Client::new();
        let paper = paper();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let names: Vec<String> = ladder
            .run(&json!({ "summary": "supported review" }), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(!names.contains(&"citation".to_string()));
    }

    #[tokio::test]
    async fn role_aware_ladder_includes_citation_for_citation_role() {
        let ladder = VerifierLadder::standard_for_config(
            &["json_schema".to_string(), "citation".to_string()],
            Some(json!({ "type": "object" })),
        );
        let http = reqwest::Client::new();
        let paper = paper();
        let ctx = VerifierContext::for_paper(&paper, &http);

        let names: Vec<String> = ladder
            .run(&json!({ "entries": [] }), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(names.contains(&"citation".to_string()));
    }

    #[tokio::test]
    async fn standard_ladder_accepts_non_paper_subjects_without_paper_extract() {
        let ladder = VerifierLadder::standard_for_config(
            &["json_schema".to_string()],
            Some(json!({ "type": "object" })),
        );
        let http = reqwest::Client::new();
        let subject = json!({
            "kind": "codebase",
            "id": "repo-agenthero-demo",
            "title": "AgentHero Demo",
            "summary": "A small codebase artifact."
        });
        let ctx = VerifierContext::new("codebase", subject, &http);

        let names: Vec<String> = ladder
            .run(&json!({ "summary": "supported review" }), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(names.contains(&"metadata".to_string()));
        assert!(names.contains(&"support".to_string()));
    }

    #[tokio::test]
    async fn citation_verifier_reports_unsupported_for_non_paper_subjects() {
        let verifier = CitationVerifier::new();
        let http = reqwest::Client::new();
        let subject = json!({ "kind": "codebase", "id": "repo-agenthero-demo" });
        let ctx = VerifierContext::new("codebase", subject, &http);

        let result = verifier.verify(&json!({}), &ctx).await;

        assert_eq!(result.status, grokrxiv_schemas::VerifierStatus::Warn);
        assert_eq!(result.notes["coverage_status"], "unsupported_subject");
    }
}
