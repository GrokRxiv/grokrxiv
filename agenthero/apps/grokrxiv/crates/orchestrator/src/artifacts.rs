//! App-local runtime artifact paths for GrokRxiv review renders.

use std::path::PathBuf;

use uuid::Uuid;

/// Directory where rendered artifacts for one review are written.
pub fn review_artifact_dir(review_id: Uuid) -> PathBuf {
    artifact_root()
        .join("grokrxiv")
        .join("reviews")
        .join(review_id.to_string())
}

/// Relative artifact reference persisted in the database.
pub fn review_artifact_ref(review_id: Uuid) -> String {
    format!("{}/grokrxiv/reviews/{review_id}", artifact_root_display())
}

fn artifact_root() -> PathBuf {
    std::env::var("AGENTHERO_ARTIFACTS_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".agenthero").join("artifacts"))
}

fn artifact_root_display() -> String {
    std::env::var("AGENTHERO_ARTIFACTS_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ".agenthero/artifacts".to_string())
}
