//! `PaperArtifacts` — façade routing per-file writes to Tier 1 (Git) or one
//! of the five Tier-2 Supabase Object Storage buckets, then returning a
//! [`PersistedPointer`] for the orchestrator to write into Postgres.
//!
//! Bucket layout (operator-locked):
//!
//! | Bucket | Contents |
//! |---|---|
//! | `raw-pdfs` | `<arxiv_id>.pdf` |
//! | `raw-source` | `<arxiv_id>.tar.gz` |
//! | `extracted-markdown` | `<arxiv_id>.md` (only when `body.md` > 1MB) |
//! | `extracted-json` | `<arxiv_id>/semantic_ast.json`, `vlm_raw.json`, large `theorem_graph.json`, `figures/...` |
//! | `embeddings` | `<arxiv_id>.bin` |
//! | `review-artifacts` | `<review_id>.json` and `<review_id>/tool_call_log.jsonl` |

use std::collections::HashMap;

use anyhow::Result;
use tracing::{debug, info};

use crate::artifact_bundle::ArtifactBundle;
use crate::git_store::GitArtifactStore;
use crate::supabase_storage::SupabaseStorage;

pub const RAW_PDFS_BUCKET: &str = "raw-pdfs";
pub const RAW_SOURCE_BUCKET: &str = "raw-source";
pub const EXTRACTED_MARKDOWN_BUCKET: &str = "extracted-markdown";
pub const EXTRACTED_JSON_BUCKET: &str = "extracted-json";
pub const EMBEDDINGS_BUCKET: &str = "embeddings";
pub const REVIEW_ARTIFACTS_BUCKET: &str = "review-artifacts";

/// Threshold above which `theorem_graph.json` is routed to Tier 2.
pub const TIER1_JSON_MAX_BYTES: usize = 200 * 1024;
/// Threshold above which `body.md` is routed to `extracted-markdown` instead
/// of the Tier-1 Git repo.
pub const TIER1_MD_MAX_BYTES: usize = 1024 * 1024;

/// Per-file routing destination, recorded for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierDecision {
    Tier1Git(String),
    Tier2Storage { bucket: &'static str, key: String },
}

/// Returned to the orchestrator after persistence so it can write the matching
/// row in `paper_assets`.
#[derive(Debug, Clone)]
pub struct PersistedPointer {
    pub paper_id: String,
    pub arxiv_id: String,
    /// Tier-1 path inside grokrxiv-data, e.g. `papers/2605.00403`.
    pub git_path: String,
    pub git_commit_sha: Option<String>,
    /// Shared path component used across Tier-2 buckets, e.g. `2605.00403`.
    /// Each bucket's key prefix is deterministic given this + the artifact
    /// kind (see the table above).
    pub storage_prefix: String,
    pub extraction_cost_usd: Option<f64>,
    pub routed: Vec<(String, TierDecision)>,
}

pub struct PaperArtifacts {
    pub git: GitArtifactStore,
    pub storage: Option<SupabaseStorage>,
}

impl PaperArtifacts {
    pub fn new(git: GitArtifactStore, storage: Option<SupabaseStorage>) -> Self {
        Self { git, storage }
    }

    pub async fn persist(
        &self,
        paper_id: String,
        bundle: ArtifactBundle,
        stages: &[&str],
        extraction_cost_usd: Option<f64>,
    ) -> Result<PersistedPointer> {
        let arxiv_id = bundle.arxiv_id.clone();
        let mut git_files: HashMap<String, Vec<u8>> = HashMap::new();
        let mut routed: Vec<(String, TierDecision)> = Vec::new();

        // ===== Always-Tier-1 JSON + small markdown =====
        if let Some(v) = &bundle.metadata {
            git_files.insert("metadata.json".into(), serde_json::to_vec_pretty(v)?);
            routed.push((
                "metadata.json".into(),
                TierDecision::Tier1Git("metadata.json".into()),
            ));
        }
        if let Some(v) = &bundle.source_manifest {
            git_files.insert("source_manifest.json".into(), serde_json::to_vec_pretty(v)?);
            routed.push((
                "source_manifest.json".into(),
                TierDecision::Tier1Git("source_manifest.json".into()),
            ));
        }
        if let Some(v) = &bundle.sections {
            git_files.insert("sections.json".into(), serde_json::to_vec_pretty(v)?);
            routed.push((
                "sections.json".into(),
                TierDecision::Tier1Git("sections.json".into()),
            ));
        }
        if let Some(v) = &bundle.equations {
            git_files.insert("equations.json".into(), serde_json::to_vec_pretty(v)?);
            routed.push((
                "equations.json".into(),
                TierDecision::Tier1Git("equations.json".into()),
            ));
        }
        if let Some(v) = &bundle.references {
            git_files.insert("references.json".into(), serde_json::to_vec_pretty(v)?);
            routed.push((
                "references.json".into(),
                TierDecision::Tier1Git("references.json".into()),
            ));
        }
        if let Some(v) = &bundle.citation_validation_report {
            git_files.insert(
                "citation_validation_report.json".into(),
                serde_json::to_vec_pretty(v)?,
            );
            routed.push((
                "citation_validation_report.json".into(),
                TierDecision::Tier1Git("citation_validation_report.json".into()),
            ));
        }
        if let Some(v) = &bundle.citation_validation_adjudication {
            git_files.insert(
                "citation_validation_adjudication.json".into(),
                serde_json::to_vec_pretty(v)?,
            );
            routed.push((
                "citation_validation_adjudication.json".into(),
                TierDecision::Tier1Git("citation_validation_adjudication.json".into()),
            ));
        }
        if let Some(v) = &bundle.extraction_report {
            git_files.insert(
                "extraction_report.json".into(),
                serde_json::to_vec_pretty(v)?,
            );
            routed.push((
                "extraction_report.json".into(),
                TierDecision::Tier1Git("extraction_report.json".into()),
            ));
        }

        // ===== body.md: Tier 1 if small, else extracted-markdown bucket =====
        let mut body_md_in_storage = false;
        let mut body_md_for_storage: Option<Vec<u8>> = None;
        if let Some(s) = &bundle.body_markdown {
            let bytes = s.as_bytes().to_vec();
            if bytes.len() > TIER1_MD_MAX_BYTES {
                debug!(size = bytes.len(), "body.md too large for Tier 1");
                body_md_in_storage = true;
                routed.push((
                    "body.md".into(),
                    TierDecision::Tier2Storage {
                        bucket: EXTRACTED_MARKDOWN_BUCKET,
                        key: format!("{arxiv_id}.md"),
                    },
                ));
                body_md_for_storage = Some(bytes);
            } else {
                routed.push(("body.md".into(), TierDecision::Tier1Git("body.md".into())));
                git_files.insert("body.md".into(), bytes);
            }
        }

        // ===== theorem_graph.json: Tier 1 if small, else extracted-json bucket =====
        let mut theorem_graph_for_storage: Option<Vec<u8>> = None;
        if let Some(v) = &bundle.theorem_graph {
            let bytes = serde_json::to_vec_pretty(v)?;
            if bytes.len() <= TIER1_JSON_MAX_BYTES {
                routed.push((
                    "theorem_graph.json".into(),
                    TierDecision::Tier1Git("theorem_graph.json".into()),
                ));
                git_files.insert("theorem_graph.json".into(), bytes);
            } else {
                debug!(size = bytes.len(), "theorem_graph too large for Tier 1");
                routed.push((
                    "theorem_graph.json".into(),
                    TierDecision::Tier2Storage {
                        bucket: EXTRACTED_JSON_BUCKET,
                        key: format!("{arxiv_id}/theorem_graph.json"),
                    },
                ));
                theorem_graph_for_storage = Some(bytes);
            }
        }

        // review_input.json: entry-point referencing the routing above
        let review_input = bundle.to_review_input(body_md_in_storage);
        git_files.insert(
            "review_input.json".into(),
            serde_json::to_vec_pretty(&review_input)?,
        );
        routed.push((
            "review_input.json".into(),
            TierDecision::Tier1Git("review_input.json".into()),
        ));

        // ===== Tier 1 write + commit =====
        self.git.write_paper_artifacts(&arxiv_id, git_files)?;
        let sha = Some(self.git.commit_and_push(&arxiv_id, stages)?);

        // ===== Tier 2 uploads, routed per bucket =====
        if let Some(storage) = &self.storage {
            if let Some(bytes) = &bundle.original_pdf {
                let key = format!("{arxiv_id}.pdf");
                storage
                    .put_object(RAW_PDFS_BUCKET, &key, bytes.clone(), "application/pdf")
                    .await?;
                routed.push((
                    "original.pdf".into(),
                    TierDecision::Tier2Storage {
                        bucket: RAW_PDFS_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = &bundle.source_tarball {
                let key = format!("{arxiv_id}.tar.gz");
                storage
                    .put_object(RAW_SOURCE_BUCKET, &key, bytes.clone(), "application/gzip")
                    .await?;
                routed.push((
                    "source.tar.gz".into(),
                    TierDecision::Tier2Storage {
                        bucket: RAW_SOURCE_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = body_md_for_storage {
                let key = format!("{arxiv_id}.md");
                storage
                    .put_object(EXTRACTED_MARKDOWN_BUCKET, &key, bytes, "text/markdown")
                    .await?;
            }
            if let Some(bytes) = &bundle.semantic_ast {
                let key = format!("{arxiv_id}/semantic_ast.json");
                storage
                    .put_object(
                        EXTRACTED_JSON_BUCKET,
                        &key,
                        bytes.clone(),
                        "application/json",
                    )
                    .await?;
                routed.push((
                    "semantic_ast.json".into(),
                    TierDecision::Tier2Storage {
                        bucket: EXTRACTED_JSON_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = &bundle.vlm_raw {
                let key = format!("{arxiv_id}/vlm_raw.json");
                storage
                    .put_object(
                        EXTRACTED_JSON_BUCKET,
                        &key,
                        bytes.clone(),
                        "application/json",
                    )
                    .await?;
                routed.push((
                    "vlm_raw.json".into(),
                    TierDecision::Tier2Storage {
                        bucket: EXTRACTED_JSON_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = theorem_graph_for_storage {
                let key = format!("{arxiv_id}/theorem_graph.json");
                storage
                    .put_object(EXTRACTED_JSON_BUCKET, &key, bytes, "application/json")
                    .await?;
            }
            for (name, bytes) in &bundle.figures {
                let key = format!("{arxiv_id}/figures/{name}");
                let content_type = if name.ends_with(".png") {
                    "image/png"
                } else if name.ends_with(".pdf") {
                    "application/pdf"
                } else {
                    "application/octet-stream"
                };
                storage
                    .put_object(EXTRACTED_JSON_BUCKET, &key, bytes.clone(), content_type)
                    .await?;
                routed.push((
                    format!("figures/{name}"),
                    TierDecision::Tier2Storage {
                        bucket: EXTRACTED_JSON_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = &bundle.embeddings {
                let key = format!("{arxiv_id}.bin");
                storage
                    .put_object(
                        EMBEDDINGS_BUCKET,
                        &key,
                        bytes.clone(),
                        "application/octet-stream",
                    )
                    .await?;
                routed.push((
                    "embeddings.bin".into(),
                    TierDecision::Tier2Storage {
                        bucket: EMBEDDINGS_BUCKET,
                        key,
                    },
                ));
            }
            if let Some(bytes) = &bundle.tool_call_log {
                // Extraction tool_call_log goes under review-artifacts keyed by
                // arxiv_id (review-bound logs use the review_id form, handled
                // by the orchestrator on review persistence).
                let key = format!("{arxiv_id}/tool_call_log.jsonl");
                storage
                    .put_object(
                        REVIEW_ARTIFACTS_BUCKET,
                        &key,
                        bytes.clone(),
                        "application/x-ndjson",
                    )
                    .await?;
                routed.push((
                    "tool_call_log.jsonl".into(),
                    TierDecision::Tier2Storage {
                        bucket: REVIEW_ARTIFACTS_BUCKET,
                        key,
                    },
                ));
            }
        }

        let pointer = PersistedPointer {
            paper_id,
            arxiv_id: arxiv_id.clone(),
            git_path: format!("papers/{arxiv_id}"),
            git_commit_sha: sha,
            storage_prefix: arxiv_id.clone(),
            extraction_cost_usd,
            routed,
        };
        info!(arxiv_id = %arxiv_id, git_path = %pointer.git_path, "PaperArtifacts persisted");
        Ok(pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn routes_small_to_git_large_to_storage() -> Result<()> {
        let work = TempDir::new()?;
        let git_path = work.path().join("data");
        let git = GitArtifactStore::open_or_clone(git_path.clone(), None)?;
        let paper = PaperArtifacts::new(git, None);

        let mut bundle = ArtifactBundle::new("2605.00403");
        let small = serde_json::json!({ "nodes": vec!["x"; 100] });
        bundle.theorem_graph = Some(small);
        bundle.metadata = Some(serde_json::json!({ "arxiv_id": "2605.00403" }));

        let p = paper
            .persist("paper-uuid".into(), bundle, &["stage1"], Some(0.10))
            .await?;
        let theorem_decision = p
            .routed
            .iter()
            .find(|(name, _)| name == "theorem_graph.json")
            .unwrap();
        assert!(matches!(theorem_decision.1, TierDecision::Tier1Git(_)));

        let mut bundle2 = ArtifactBundle::new("2605.00404");
        let huge_list: Vec<String> = (0..50_000).map(|i| format!("node-{i}-padding")).collect();
        bundle2.theorem_graph = Some(serde_json::json!({ "nodes": huge_list }));
        bundle2.metadata = Some(serde_json::json!({ "arxiv_id": "2605.00404" }));

        let p2 = paper
            .persist("paper-uuid-2".into(), bundle2, &["stage1"], Some(0.10))
            .await?;
        let theorem_decision2 = p2
            .routed
            .iter()
            .find(|(name, _)| name == "theorem_graph.json")
            .unwrap();
        match &theorem_decision2.1 {
            TierDecision::Tier2Storage { bucket, key } => {
                assert_eq!(*bucket, EXTRACTED_JSON_BUCKET);
                assert_eq!(key, "2605.00404/theorem_graph.json");
            }
            other => panic!("expected Tier2 for 300KB theorem_graph, got {other:?}"),
        }

        Ok(())
    }
}
