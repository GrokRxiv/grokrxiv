"use client";

import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { AgentReviewDetails } from "@/components/agent-review-details";
import { VerifierStatusBadge } from "@/components/review-status-badge";
import type { AgentOutput, AgentRole } from "@/lib/types";

const ROLE_LABEL: Record<AgentRole, string> = {
  summary: "Summary",
  technical_correctness: "Technical correctness",
  novelty: "Novelty",
  reproducibility: "Reproducibility",
  citation: "Citation",
  meta_reviewer: "Overall reviewer",
};

const ROLE_ORDER: AgentRole[] = [
  "summary",
  "technical_correctness",
  "novelty",
  "reproducibility",
  "citation",
  "meta_reviewer",
];

export function AgentAccordion({ agents }: { agents: AgentOutput[] }) {
  const ordered = [...agents].sort(
    (a, b) => ROLE_ORDER.indexOf(a.role) - ROLE_ORDER.indexOf(b.role),
  );
  return (
    <Accordion type="multiple" className="flex w-full flex-col gap-2">
      {ordered.map((agent) => (
        <AccordionItem
          key={agent.role}
          value={agent.role}
          className="rounded-lg border border-[color:var(--color-border)] bg-slate-900/40 px-4 [&]:border-b"
        >
          <AccordionTrigger className="py-3 hover:no-underline">
            <div className="flex flex-1 flex-wrap items-center justify-between gap-3 pr-2">
              <div className="flex flex-wrap items-center gap-3">
                <span className="text-sm font-semibold text-slate-100">
                  {ROLE_LABEL[agent.role] ?? agent.role}
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
