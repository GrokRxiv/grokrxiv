//! GrokRxiv typed artifact schemas.
//!
//! Every artifact that flows between pipeline stages (ingest → review DAG →
//! verifier → render → publisher) is modeled here as a `serde`-friendly Rust
//! struct or enum. All public types derive `Serialize`, `Deserialize`,
//! `Debug`, and `Clone`. Enabling the `json-schema` Cargo feature additionally
//! derives [`schemars::JsonSchema`] so Supabase migration tooling can emit
//! JSON Schema files from the source of truth.
//!
//! ## Conventions
//!
//! * Enums use `#[serde(rename_all = "snake_case")]` so wire formats stay
//!   human-readable and stable across providers.
//! * The literal field `abstract` is renamed via `serde` because `abstract` is
//!   a reserved word in Rust.
//! * Severities and recommendations are exhaustive enums (no free-form
//!   strings) so the verifier can pattern-match without parsing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "json-schema")]
use schemars::JsonSchema;

// ---------------------------------------------------------------------------
// Paper extraction (ingest → review DAG input)
// ---------------------------------------------------------------------------

/// Result of running the ingest pipeline on a single arXiv paper.
///
/// Produced by `grokrxiv-ingest` and consumed by every review agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct PaperExtract {
    /// arXiv identifier including version suffix (e.g. `2401.12345v2`).
    pub arxiv_id: String,
    /// Paper title as parsed from the PDF / arXiv metadata.
    pub title: String,
    /// Ordered list of authors with affiliations where available.
    pub authors: Vec<Author>,
    /// Paper abstract.
    #[serde(rename = "abstract")]
    pub abstract_: String,
    /// Primary arXiv category, e.g. `cs.LG`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub field: Option<String>,
    /// Extracted body sections in document order.
    pub sections: Vec<Section>,
    /// Figure references with captions and storage hints.
    pub figures: Vec<FigureRef>,
    /// Bibliography entries as both raw text and parsed identifiers.
    pub bibliography: Vec<Citation>,
    /// Which source the extract was built from: `"tex"` when the LaTeX source
    /// bundle was available on arXiv, `"pdf"` when we fell back to PDF text
    /// extraction. None on the legacy code path for backward compat.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_format: Option<String>,
}

/// One author block on a paper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct Author {
    /// Author full name as it appears on the paper.
    pub name: String,
    /// Institutional affiliation if parseable.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub affiliation: Option<String>,
    /// Contact email when listed on the manuscript.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
}

/// A single section in the paper body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct Section {
    /// Section heading (e.g. `2. Methods`).
    pub heading: String,
    /// Section body rendered to Markdown.
    pub body_markdown: String,
}

/// Reference to a figure extracted from the PDF.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct FigureRef {
    /// Figure caption.
    pub caption: String,
    /// 1-indexed page number where the figure appears.
    pub page: u32,
    /// Optional storage path for the extracted figure image.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_path: Option<String>,
}

/// A bibliography entry; one or more of `doi` / `arxiv_id` / `title` may be
/// resolved by the citation verifier later.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct Citation {
    /// The original raw bibliography string as it appeared in the paper.
    pub raw: String,
    /// Resolved DOI, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub doi: Option<String>,
    /// Resolved arXiv id, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub arxiv_id: Option<String>,
    /// Resolved title, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent role identifiers
// ---------------------------------------------------------------------------

/// Distinct review-agent roles in the DAG. Persisted on `review_agents.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Plain-language summary specialist.
    Summary,
    /// Technical / mathematical correctness specialist.
    TechnicalCorrectness,
    /// Novelty and related-work specialist.
    Novelty,
    /// Reproducibility (code/data/instructions) specialist.
    Reproducibility,
    /// Citation existence/quality specialist.
    Citation,
    /// Synthesizing meta-reviewer.
    MetaReviewer,
}

// ---------------------------------------------------------------------------
// Per-agent typed outputs
// ---------------------------------------------------------------------------

/// Output of the summary agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct SummaryReview {
    /// Plain-language summary suitable for a general audience.
    pub plain_summary: String,
    /// Bullet list of the paper's stated contributions.
    pub contributions: Vec<String>,
    /// Bullet list of the methods used.
    pub methods: Vec<String>,
}

/// Output of the technical-correctness agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct TechnicalReview {
    /// Per-claim assessments.
    pub claims: Vec<ClaimAssessment>,
    /// 1..=5 soundness score.
    pub soundness_score: u8,
    /// Free-form notes from the reviewer agent.
    pub notes: String,
}

/// A single claim's assessment as part of [`TechnicalReview`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct ClaimAssessment {
    /// The claim being assessed, paraphrased from the paper.
    pub claim: String,
    /// Why the agent considers the claim supported or unsupported.
    pub support: String,
    /// Severity if the claim is unsound; informational otherwise.
    pub severity: Severity,
    /// Best-effort pointer to the location in the paper, e.g. `Section 3.2`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub location: Option<String>,
}

/// Severity scale shared across claim/citation/repro assessments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational; no action needed.
    Info,
    /// Minor issue.
    Minor,
    /// Major issue requiring revision.
    Major,
    /// Critical flaw.
    Critical,
}

/// Output of the novelty agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct NoveltyReview {
    /// Closely-related prior work the agent surfaced.
    pub related_work: Vec<RelatedWork>,
    /// Free-form description of the delta vs prior work.
    pub delta: String,
    /// 1..=5 novelty score.
    pub novelty_score: u8,
}

/// A related-work entry produced by the novelty agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct RelatedWork {
    /// Title of the related paper.
    pub title: String,
    /// arXiv id when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub arxiv_id: Option<String>,
    /// DOI when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub doi: Option<String>,
    /// Why this is similar / where the overlap lies.
    pub overlap: String,
}

/// Output of the reproducibility agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct ReproducibilityReview {
    /// Code is available (link or repo cited).
    pub code_available: bool,
    /// Data is available (link or dataset cited).
    pub data_available: bool,
    /// Instructions are clear enough to reproduce.
    pub instructions_clear: bool,
    /// List of specific reproducibility issues.
    pub issues: Vec<String>,
    /// 1..=5 reproducibility score.
    pub repro_score: u8,
}

/// Output of the citation agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct CitationReview {
    /// Citations that could not be resolved against Crossref / Semantic
    /// Scholar / arXiv.
    pub unresolved: Vec<Citation>,
    /// Citations that resolved but look suspicious (year mismatch, retracted,
    /// etc).
    pub suspicious: Vec<Citation>,
    /// Number of citations that passed verification.
    pub ok_count: u32,
}

/// Final synthesized review produced by the meta-reviewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct MetaReview {
    /// Overall executive summary of the paper.
    pub summary: String,
    /// Bullet list of strengths.
    pub strengths: Vec<String>,
    /// Bullet list of weaknesses.
    pub weaknesses: Vec<String>,
    /// Questions for the authors.
    pub questions: Vec<String>,
    /// Final recommendation.
    pub recommendation: Recommendation,
    /// 0.0..=1.0 confidence in the recommendation.
    pub confidence: f32,
}

/// Reviewer recommendation, modeled after typical conference reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// Accept as-is.
    Accept,
    /// Accept after minor revisions.
    MinorRevision,
    /// Major revisions required.
    MajorRevision,
    /// Reject.
    Reject,
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

/// Outcome of a verifier rung in the ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct VerifierResult {
    /// Pass/warn/fail status.
    pub status: VerifierStatus,
    /// Verifier-specific structured notes (free-form JSON to avoid coupling
    /// every consumer to every verifier's payload shape).
    pub notes: serde_json::Value,
}

/// Status from a verifier rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum VerifierStatus {
    /// Verifier passed without notes.
    Pass,
    /// Verifier passed with non-blocking notes.
    Warn,
    /// Verifier failed; pipeline should retry or escalate.
    Fail,
}

// ---------------------------------------------------------------------------
// Job state (mirrors `jobs` table)
// ---------------------------------------------------------------------------

/// In-memory mirror of a row in the `jobs` table used by the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
pub struct Job {
    /// Job primary key.
    pub id: Uuid,
    /// What kind of work is queued.
    pub kind: JobKind,
    /// Reference to the entity being acted on (paper / review id).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ref_id: Option<Uuid>,
    /// Current lifecycle state.
    pub state: JobState,
    /// Retry attempt number (0 = first try).
    pub attempt: i32,
    /// Error message captured on the most recent failed attempt.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    /// When the job most recently transitioned to `running`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub started_at: Option<DateTime<Utc>>,
    /// When the job transitioned to `done` or `failed`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub finished_at: Option<DateTime<Utc>>,
}

/// Kinds of pipeline jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Pull paper from arXiv and extract text.
    Ingest,
    /// Run the multi-agent review DAG.
    Review,
    /// Render artifacts (HTML/MD/LaTeX/PDF/zip).
    Render,
    /// Publish to Supabase Storage and open a GitHub PR.
    Publish,
    /// Fast single-pass landing-page preview.
    Preview,
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting to be picked up.
    Queued,
    /// A worker is currently processing it.
    Running,
    /// Completed successfully.
    Done,
    /// Failed terminally (no more retries).
    Failed,
}

// ---------------------------------------------------------------------------
// Review status (mirrors `reviews.status` column)
// ---------------------------------------------------------------------------

/// Lifecycle state of a single review run.
///
/// The pipeline runs through:
/// `draft` → `awaiting_moderation` → (human approves) → `pr_open` → `published`.
/// `in_review` is used while the review DAG is mid-flight; `corrected` and
/// `withdrawn` are terminal post-publication states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// Initial state when the review row is created.
    Draft,
    /// Pipeline finished private artifact generation; awaiting human
    /// moderation. The publisher does NOT auto-open a PR from this state.
    AwaitingModeration,
    /// Review DAG is currently running.
    InReview,
    /// Moderator approved; publisher has opened a PR against the canonical
    /// reviews repository.
    PrOpen,
    /// Reviews PR was merged; review is publicly visible.
    Published,
    /// A correction was published after the initial review.
    Corrected,
    /// Review was withdrawn after publication.
    Withdrawn,
}

// ---------------------------------------------------------------------------
// Public-facing constants
// ---------------------------------------------------------------------------

/// Reserved for the dedicated legal page only — intentionally empty so the
/// constant cannot accidentally render into headers, footers, banners, review
/// bodies, or PR bodies. The web UI surfaces the full legal text at `/legal`
/// and nowhere else.
pub const PUBLIC_DISCLAIMER: &str = "";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_extract_roundtrip_renames_abstract() {
        let extract = PaperExtract {
            arxiv_id: "2401.12345v1".into(),
            title: "Toward Typed Peer Review".into(),
            authors: vec![Author {
                name: "A. Reviewer".into(),
                affiliation: None,
                email: None,
            }],
            abstract_: "We show that types help.".into(),
            field: Some("cs.LG".into()),
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        };
        let json = serde_json::to_value(&extract).unwrap();
        assert_eq!(json["abstract"], "We show that types help.");
        assert!(json.get("abstract_").is_none());
        let parsed: PaperExtract = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.title, extract.title);
    }

    #[test]
    fn enums_use_snake_case() {
        let r = serde_json::to_value(Recommendation::MinorRevision).unwrap();
        assert_eq!(r, serde_json::Value::String("minor_revision".into()));
        let s = serde_json::to_value(Severity::Critical).unwrap();
        assert_eq!(s, serde_json::Value::String("critical".into()));
        let js = serde_json::to_value(JobState::Queued).unwrap();
        assert_eq!(js, serde_json::Value::String("queued".into()));
    }

    #[test]
    fn agent_role_serializes_snake_case() {
        let r = serde_json::to_value(AgentRole::TechnicalCorrectness).unwrap();
        assert_eq!(r, serde_json::Value::String("technical_correctness".into()));
    }

    #[test]
    fn review_status_includes_awaiting_moderation() {
        let r = serde_json::to_value(ReviewStatus::AwaitingModeration).unwrap();
        assert_eq!(r, serde_json::Value::String("awaiting_moderation".into()));
        let r = serde_json::to_value(ReviewStatus::PrOpen).unwrap();
        assert_eq!(r, serde_json::Value::String("pr_open".into()));
    }

    #[test]
    fn disclaimer_is_empty_outside_of_legal_page() {
        // The disclaimer string is intentionally empty in the shared schema —
        // the only place it surfaces is the dedicated `/legal` page on the
        // web app. Anything that previously rendered this constant into the
        // public UI should no longer do so.
        assert_eq!(PUBLIC_DISCLAIMER, "");
    }
}
