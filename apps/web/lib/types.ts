// Mirrors of the Rust `crates/schemas` serde structs. Keep field names in sync.

export type ReviewStatus =
  | "draft"
  | "awaiting_moderation"
  | "in_review"
  | "pr_open"
  | "published"
  | "corrected"
  | "withdrawn";

// Statuses visible to anonymous public readers / public APIs.
export const PUBLIC_REVIEW_STATUSES: readonly ReviewStatus[] = [
  "published",
  "corrected",
] as const;

export type Recommendation =
  | "accept"
  | "minor_revision"
  | "major_revision"
  | "reject";

export type VerifierStatus = "pass" | "warn" | "fail";

export type AgentRole =
  | "summary"
  | "technical_correctness"
  | "novelty"
  | "reproducibility"
  | "citation"
  | "meta_reviewer";

export interface Author {
  name: string;
  affiliation?: string;
  email?: string;
}

export interface Paper {
  id: string;
  arxiv_id: string;
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
  recommendation: Recommendation;
  confidence: number;
}

export interface AgentOutput {
  role: AgentRole;
  model: string;
  output: unknown;
  verifier_status: VerifierStatus;
}

export interface ReviewSummary {
  id: string;
  paper_id: string;
  status: ReviewStatus;
  github_pr_url?: string;
  github_review_url?: string;
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
