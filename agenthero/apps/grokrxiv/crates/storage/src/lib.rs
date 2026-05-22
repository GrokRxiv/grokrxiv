//! grokrxiv-storage — 3-tier persistence SDK.
//!
//! Tier 1: `grokrxiv-data` Git repo (small, versioned, review-critical artifacts).
//! Tier 2: Supabase Object Storage bucket `paper-assets` (bulky binaries).
//! Tier 3: Supabase Postgres (pointers + status; written by the orchestrator,
//!         not by this crate).
//!
//! The [`PaperArtifacts`] façade routes per-file writes to Tier 1 or Tier 2
//! based on file kind and size, returning [`PersistedPointer`] for the
//! orchestrator to persist to Postgres.

pub mod artifact_bundle;
pub mod git_store;
pub mod paper_artifacts;
pub mod supabase_storage;

pub use artifact_bundle::{ArtifactBundle, ReviewInput};
pub use git_store::GitArtifactStore;
pub use paper_artifacts::{PaperArtifacts, PersistedPointer, TierDecision};
pub use supabase_storage::SupabaseStorage;
