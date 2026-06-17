"use client";

import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { AgentReviewDetails } from "@/components/agent-review-details";
import { VerifierStatusBadge } from "@/components/review-status-badge";
import type { AgentOutput, KnownAgentRole } from "@/lib/types";

const ROLE_LABEL: Record<KnownAgentRole, string> = {
  summary: "Summary",
  technical_correctness: "Technical correctness",
  novelty: "Novelty",
  reproducibility: "Reproducibility",
  citation: "Citation",
  meta_reviewer: "Overall reviewer",
};

// Human-readable display names for the review-loop / formalization nodes (whose
// `role` is the raw snake_case node id). Anything not listed here falls back to
// `humanizeRole`, so every check renders as prose rather than an internal id.
const NODE_LABEL: Record<string, string> = {
  claim_extractor: "Claim extractor",
  paper_math_source_collector: "Paper math sources",
  knowledge_graph_builder: "Knowledge graph",
  semantic_category_mapper: "Semantic category mapping",
  proof_obligation_generator: "Proof obligations",
  semantic_adequacy_checker: "Lean faithfulness adequacy",
  citation_validation: "Citation validation",
  citation_validation_adjudication: "Citation adjudication",
  pr_fixer: "PR fixer",
  pr_review_fix_code: "PR review fix",
  lean_review_fix_code: "Lean review fix",
  lean_faithfulness_check: "Lean faithfulness check",
  policy_gate: "Policy gate",
  review_loop_report: "Review loop report",
  publish_decision: "Publish decision",
  bundle_completeness: "Bundle completeness",
};

// Known acronyms that should stay upper-cased when humanizing an unmapped id.
const ACRONYMS = new Set(["pr", "llm", "ir", "dag", "pdf", "url", "doi", "ci", "id"]);

function humanizeRole(role: string): string {
  const words = role.split(/[_\-\s]+/).filter(Boolean);
  if (words.length === 0) return role;
  return words
    .map((word, index) => {
      if (ACRONYMS.has(word.toLowerCase())) return word.toUpperCase();
      const cased = word.charAt(0).toUpperCase() + word.slice(1);
      return index === 0 ? cased : word;
    })
    .join(" ");
}

const ROLE_ORDER: KnownAgentRole[] = [
  "summary",
  "technical_correctness",
  "novelty",
  "reproducibility",
  "citation",
  "meta_reviewer",
];

export function AgentAccordion({ agents }: { agents: AgentOutput[] }) {
  const ordered = [...agents].sort((a, b) => roleRank(a.role) - roleRank(b.role));
  return (
    <Accordion type="multiple" className="flex w-full flex-col gap-2">
      {ordered.map((agent) => (
        <AccordionItem
          key={`${agent.dag_type ?? "paper-review"}:${agent.node_id ?? agent.role}`}
          value={`${agent.dag_type ?? "paper-review"}:${agent.node_id ?? agent.role}`}
          className="rounded-lg border border-[color:var(--color-border)] bg-slate-900/40 px-4 [&]:border-b"
        >
          <AccordionTrigger className="py-3 hover:no-underline">
            <div className="flex flex-1 flex-wrap items-center justify-between gap-3 pr-2">
              <div className="flex flex-wrap items-center gap-3">
                <span className="text-sm font-semibold text-slate-100">
                  {roleLabel(agent.role)}
                </span>
              </div>
              <VerifierStatusBadge
                status={displayVerifierStatus(agent).status}
                label={displayVerifierStatus(agent).label}
              />
            </div>
          </AccordionTrigger>
          <AccordionContent>
            <AgentReviewDetails
              role={agent.role}
              output={agent.output}
              verifierNotes={agent.verifier_notes}
            />
          </AccordionContent>
        </AccordionItem>
      ))}
    </Accordion>
  );
}

function roleRank(role: string): number {
  const known = ROLE_ORDER.indexOf(role as KnownAgentRole);
  return known >= 0 ? known : ROLE_ORDER.length;
}

function roleLabel(role: string): string {
  return ROLE_LABEL[role as KnownAgentRole] ?? NODE_LABEL[role] ?? humanizeRole(role);
}

function displayVerifierStatus(agent: AgentOutput): {
  status: AgentOutput["verifier_status"];
  label?: string;
} {
  if (agent.role === "citation" && citationWasNotChecked(agent.verifier_notes)) {
    return { status: "fail", label: "Not checked" };
  }
  return { status: agent.verifier_status };
}

function citationWasNotChecked(verifierNotes: unknown): boolean {
  if (!isRecord(verifierNotes)) return false;
  const citation = recordField(verifierNotes, "citation");
  const notes = citation ? recordField(citation, "notes") : verifierNotes;
  if (!isRecord(notes)) return false;
  return notes.checked === 0 || notes.coverage_status === "not_checked";
}

function recordField(
  record: Record<string, unknown>,
  key: string,
): Record<string, unknown> | null {
  const value = record[key];
  return isRecord(value) ? value : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
