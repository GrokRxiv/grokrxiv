//! Per-paper artifact bundle and `review_input.json` schema.
//!
//! `ArtifactBundle` is the in-memory holder used by the ingest pipeline to
//! accumulate per-stage outputs. `ReviewInput` is the serialised entry-point
//! file the orchestrator reads (Tier-1 path + Tier-2 URIs).

use serde::{Deserialize, Serialize};

use crate::paper_artifacts::{
    EXTRACTED_JSON_BUCKET, EXTRACTED_MARKDOWN_BUCKET, EMBEDDINGS_BUCKET, RAW_PDFS_BUCKET,
    RAW_SOURCE_BUCKET,
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ArtifactBundle {
    pub arxiv_id: String,
    pub metadata: Option<serde_json::Value>,
    pub source_manifest: Option<serde_json::Value>,
    pub body_markdown: Option<String>,
    pub sections: Option<serde_json::Value>,
    pub equations: Option<serde_json::Value>,
    pub references: Option<serde_json::Value>,
    pub theorem_graph: Option<serde_json::Value>,
    pub extraction_report: Option<serde_json::Value>,
    pub semantic_ast: Option<Vec<u8>>,
    pub vlm_raw: Option<Vec<u8>>,
    pub embeddings: Option<Vec<u8>>,
    pub tool_call_log: Option<Vec<u8>>,
    pub original_pdf: Option<Vec<u8>>,
    pub source_tarball: Option<Vec<u8>>,
    pub figures: Vec<(String, Vec<u8>)>,
}

impl ArtifactBundle {
    pub fn new(arxiv_id: impl Into<String>) -> Self {
        Self {
            arxiv_id: arxiv_id.into(),
            ..Default::default()
        }
    }

    /// Build the `review_input.json` payload. Tier-1 paths are relative to the
    /// repo root; Tier-2 URIs use `supabase://<bucket>/<key>` form derived
    /// from the operator-locked 5-bucket layout.
    pub fn to_review_input(&self, body_md_in_storage: bool) -> ReviewInput {
        let p = format!("papers/{}", self.arxiv_id);
        let arxiv = &self.arxiv_id;
        let supa = |bucket: &str, key: String| format!("supabase://{bucket}/{key}");
        let body_markdown = if body_md_in_storage {
            supa(EXTRACTED_MARKDOWN_BUCKET, format!("{arxiv}.md"))
        } else {
            format!("{p}/body.md")
        };
        ReviewInput {
            schema_version: "1".to_string(),
            arxiv_id: arxiv.clone(),
            metadata: format!("{p}/metadata.json"),
            body_markdown,
            sections: format!("{p}/sections.json"),
            equations: format!("{p}/equations.json"),
            references: format!("{p}/references.json"),
            theorem_graph: format!("{p}/theorem_graph.json"),
            extraction_report: format!("{p}/extraction_report.json"),
            pdf_uri: self
                .original_pdf
                .as_ref()
                .map(|_| supa(RAW_PDFS_BUCKET, format!("{arxiv}.pdf"))),
            source_uri: self
                .source_tarball
                .as_ref()
                .map(|_| supa(RAW_SOURCE_BUCKET, format!("{arxiv}.tar.gz"))),
            semantic_ast_uri: self
                .semantic_ast
                .as_ref()
                .map(|_| supa(EXTRACTED_JSON_BUCKET, format!("{arxiv}/semantic_ast.json"))),
            vlm_raw_uri: self
                .vlm_raw
                .as_ref()
                .map(|_| supa(EXTRACTED_JSON_BUCKET, format!("{arxiv}/vlm_raw.json"))),
            embeddings_uri: self
                .embeddings
                .as_ref()
                .map(|_| supa(EMBEDDINGS_BUCKET, format!("{arxiv}.bin"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewInput {
    pub schema_version: String,
    pub arxiv_id: String,
    pub metadata: String,
    pub body_markdown: String,
    pub sections: String,
    pub equations: String,
    pub references: String,
    pub theorem_graph: String,
    pub extraction_report: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_ast_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlm_raw_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings_uri: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn data_repo_schema() -> Option<serde_json::Value> {
        let path = PathBuf::from(
            "/Users/mlong/Documents/Development/grokrxiv-data/schemas/review_input.schema.json",
        );
        if !path.exists() {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    #[test]
    fn to_review_input_matches_schema() {
        let Some(schema) = data_repo_schema() else {
            return;
        };
        let mut b = ArtifactBundle::new("2605.00403");
        b.original_pdf = Some(vec![0u8; 8]);
        b.source_tarball = Some(vec![0u8; 8]);
        b.semantic_ast = Some(vec![0u8; 8]);
        let ri = b.to_review_input(false);
        let value = serde_json::to_value(&ri).unwrap();
        let validator = jsonschema::validator_for(&schema).expect("compile review_input schema");
        let errs: Vec<String> = validator.iter_errors(&value).map(|e| e.to_string()).collect();
        if !errs.is_empty() {
            panic!("validation failed: {}", errs.join("; "));
        }
        // sanity: per-artifact buckets are baked in
        assert!(ri.pdf_uri.as_deref().unwrap().starts_with("supabase://raw-pdfs/"));
        assert!(ri
            .semantic_ast_uri
            .as_deref()
            .unwrap()
            .starts_with("supabase://extracted-json/"));
    }
}
