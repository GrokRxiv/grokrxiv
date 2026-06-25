export const APP_PROTOCOL = "agenthero.app.v1";
export const APP_ADAPTER_EVENT_PREFIX = "@@AGENTHERO_EVENT ";
export const APP_ID = "formal-proofs";
export const AGENTHERO_EVENT_TRACE_FIELDS = [
  "app_run_id",
  "dag_run_id",
  "node_id",
  "attempt",
  "node_kind",
  "tool_id",
  "manifest_hash",
  "artifact_id",
  "lease_id",
  "status",
  "exit_status",
  "duration_ms"
] as const;

export type Checker = "lean" | "haskell" | "sat" | "manual" | "unknown";
export type ClaimType =
  | "construction"
  | "countermodel"
  | "lower_bound"
  | "upper_bound"
  | "identity"
  | "proof_sketch";

export interface DagIo {
  values?: Record<string, unknown>;
  artifacts?: Record<string, ArtifactRef>;
}

export interface AppAdapterRequest {
  protocol: string;
  app: string;
  action: string;
  dag_type: string;
  args?: string[];
  input?: DagIo;
  json?: boolean;
  dry_run?: boolean;
  idempotency_key?: string;
  checkpoint?: DagExecutionReport;
}

export interface AppAdapterResponse {
  protocol: string;
  app: string;
  action: string;
  dag_type: string;
  ok: boolean;
  report?: DagExecutionReport;
  output?: Record<string, unknown>;
  error?: string | null;
}

export interface DagExecutionReport {
  dag_type: string;
  manifest_version: number;
  manifest_hash: string;
  status: "pending" | "running" | "awaiting_approval" | "ok" | "degraded" | "failed" | "skipped";
  input: {
    values: Record<string, unknown>;
    artifacts: Record<string, ArtifactRef>;
  };
  nodes: DagNodeReport[];
  outputs: {
    values: Record<string, unknown>;
    artifacts: Record<string, ArtifactRef>;
  };
  events: DagExecutionEvent[];
}

export interface DagNodeReport {
  node_id: string;
  kind: string;
  status: "pending" | "running" | "awaiting_approval" | "ok" | "degraded" | "failed" | "skipped";
  attempt?: number;
  role?: string | null;
  tool?: string | null;
  child_dag_type?: string | null;
  required?: boolean;
  executor?: string | null;
  model?: string | null;
  prompt_hash?: string | null;
  command?: string[] | null;
  exit_status?: number | null;
  inputs?: string[];
  outputs?: string[];
  input_refs?: Record<string, string>;
  output_refs?: Record<string, string>;
  diagnostic_refs?: Record<string, string>;
  policy?: Record<string, unknown>;
  warning?: string | null;
  error?: string | null;
  latency_ms?: number;
  trace?: Record<string, unknown>;
}

export interface DagExecutionEvent {
  level: "debug" | "info" | "warn" | "error";
  event_type: string;
  node_id?: string | null;
  message?: string | null;
  payload: Record<string, unknown>;
}

export interface ArtifactRef {
  uri: string;
  media_type?: string | null;
  metadata?: Record<string, unknown>;
}

export interface Candidate {
  candidate_id: string;
  lane: string;
  locked_open_problem: string;
  claim_type: ClaimType;
  object: Record<string, unknown>;
  parameters: Record<string, unknown>;
  claimed_improvement: string;
  verification_target: string;
  expected_checker: Checker;
  proposer: string;
  notes: string;
}

export interface NormalizedCandidate extends Candidate {
  verified: false;
}

export interface RejectedCandidate {
  raw: string;
  reason: string;
}

export interface VerifierCommandResult {
  command: string[];
  exit_code: number | null;
  stdout: string;
  stderr: string;
  duration_ms: number;
}

export interface VerifierResult {
  candidate_id: string;
  trusted: boolean;
  status: "verified" | "failed" | "unverified" | "checker_unavailable";
  expected_checker: Checker;
  commands: VerifierCommandResult[];
  evidence_path: string | null;
  notes: string;
}

export interface FinalReport {
  open_problem_solved: boolean;
  trusted_status: "solved" | "not_solved" | "partial" | "failed";
  locked_open_problem: string;
  verifier_commands: string[];
  failure_reason: string | null;
  partial_artifacts: string[];
  workspace: string;
}
