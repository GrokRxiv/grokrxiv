// Mirrors of the Rust `crates/schemas` serde structs. Keep field names in sync.

export type ReviewStatus =
  | "draft"
  | "awaiting_moderation"
  | "in_review"
  | "pr_open"
  | "published"
  | "corrected"
  | "withdrawn"
  | "rejected";

export type ReviewVisibility = "public" | "private";

// Statuses visible to anonymous public readers / public APIs. A `pr_open`
// review is shown with an "In Review" badge so papers reach the site as
// soon as a PR opens on the mirror repo; the webhook flips it to
// `published` (green badge) on merge. `rejected` shows a red badge plus
// the moderator's rationale from the `rejections` table.
export const PUBLIC_REVIEW_STATUSES: readonly ReviewStatus[] = [
  "pr_open",
  "published",
  "corrected",
  "rejected",
] as const;

export type Recommendation =
  | "accept"
  | "minor_revision"
  | "major_revision"
  | "reject";

export type VerifierStatus = "pass" | "warn" | "fail";

export type RevisionTargetKind =
  | "paper_tex"
  | "paper_pdf"
  | "code"
  | "data"
  | "bibliography"
  | "review_text"
  | "unknown";

export type RevisionTargetStatus =
  | "open"
  | "addressed"
  | "still_open"
  | "superseded"
  | "unknown";

export type KnownAgentRole =
  | "summary"
  | "technical_correctness"
  | "novelty"
  | "reproducibility"
  | "citation"
  | "meta_reviewer";

export type AgentRole = KnownAgentRole | string;

export interface Author {
  name: string;
  affiliation?: string;
  email?: string;
}

export interface Paper {
  id: string;
  arxiv_id: string;
  source_kind?: string | null;
  source_id?: string | null;
  source_uri?: string | null;
  source_hash?: string | null;
  source_metadata?: Record<string, unknown> | null;
  title: string;
  authors: Author[];
  abstract?: string;
  field?: string;
  ingested_at: string;
}

export interface MetaReview {
  summary: string;
  strengths: string[];
  weaknesses: string[];
  questions: string[];
  revision_targets?: RevisionTarget[];
  recommendation: Recommendation;
  confidence: number;
}

export interface RevisionTarget {
  id: string;
  weakness_index: number;
  source_role: string | null;
  target_kind: RevisionTargetKind;
  source_path: string | null;
  locator: string | null;
  evidence: string | null;
  required_update: string;
  verification_check: string;
  status: RevisionTargetStatus;
}

export interface AgentOutput {
  role: AgentRole;
  dag_type?: string | null;
  node_id?: string | null;
  agent_type?: string | null;
  model: string;
  output: unknown;
  verifier_status: VerifierStatus;
  verifier_notes?: unknown | null;
}

export interface SummaryReviewOutput {
  tldr: string | null;
  plain_language_summary: string | null;
  audience: string | null;
  key_contributions: string[];
}

export interface TechnicalClaimOutput {
  id: string | null;
  claim: string | null;
  assessment: string | null;
  severity: string | null;
  location: string | null;
  evidence: string | null;
  suggested_fix: string | null;
}

export interface TechnicalReviewOutput {
  claims: TechnicalClaimOutput[];
  overall_correctness: string | null;
  confidence: number | null;
}

export interface RelatedWorkOutput {
  citation_key: string | null;
  title: string | null;
  relation: string | null;
  delta: string | null;
}

export interface MissingReferenceOutput {
  title: string | null;
  reason: string | null;
}

export interface NoveltyReviewOutput {
  novelty_score: number | null;
  verdict: string | null;
  confidence: number | null;
  related_work: RelatedWorkOutput[];
  missing_prior_art: MissingReferenceOutput[];
}

export interface ReproducibilityEnvironmentOutput {
  hardware: string | null;
  software: string | null;
  dependencies: string[];
}

export interface ReproducibilityConcernOutput {
  area: string | null;
  description: string | null;
  severity: string | null;
}

export interface ReproducibilityReviewOutput {
  code_availability: string | null;
  code_url: string | null;
  data_availability: string | null;
  data_url: string | null;
  environment: ReproducibilityEnvironmentOutput | null;
  concerns: ReproducibilityConcernOutput[];
  reproducibility_score: number | null;
  confidence: number | null;
}

export interface CitationReferenceOutput {
  key: string | null;
  raw: string | null;
  title: string | null;
  authors: string[];
  year: number | null;
  venue: string | null;
  doi: string | null;
  arxiv_id: string | null;
  url: string | null;
}

export interface CitationEntryOutput {
  citation: CitationReferenceOutput | null;
  exists: boolean | null;
  resolved_doi: string | null;
  resolved_url: string | null;
  relevance: string | null;
  notes: string | null;
  explanation: string | null;
}

export interface CitationReviewOutput {
  entries: CitationEntryOutput[];
  missing_references: MissingReferenceOutput[];
  summary: string | null;
  confidence: number | null;
}

export interface ReviewSummary {
  id: string;
  paper_id: string;
  status: ReviewStatus;
  visibility: ReviewVisibility;
  submitted_by?: string | null;
  github_pr_url?: string;
  github_review_url?: string;
  github_comment_url?: string | null;
  gate_failure_reason?: string | null;
  gate_failure_instructions?: string | null;
  gate_failure_comment_url?: string | null;
  models_used: Record<string, string>;
  created_at: string;
  published_at?: string;
}

export interface Review extends ReviewSummary {
  meta_review: MetaReview;
  agents: AgentOutput[];
}

// Joined view returned for list endpoints / cards.
export interface ReviewWithPaper extends ReviewSummary {
  paper: Paper;
  meta_review?: MetaReview;
}

// Response from POST /api/upload (which proxies orchestrator /preview).
// Per the revised architecture, this is a *sample* review, never a real
// GrokRxiv-published peer review. The Rust orchestrator returns:
//   { is_sample: true, sample_review_id, meta_review, html, bundle_b64 }
// where `html` is a self-contained HTML string for inline preview and
// `bundle_b64` is the base64-encoded zip bundle. The client converts the
// bundle to a Blob URL for download — no Supabase Storage required for the
// preview path.
export interface SampleResponse {
  is_sample: true;
  sample_review_id: string;
  meta_review: MetaReview;
  html: string;
  bundle_b64: string;
}
