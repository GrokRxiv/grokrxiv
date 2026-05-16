"use client";

import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { Badge } from "@/components/ui/badge";
import { VerifierStatusBadge } from "@/components/review-status-badge";
import type { AgentOutput, AgentRole } from "@/lib/types";

const ROLE_LABEL: Record<AgentRole, string> = {
  summary: "Summary",
  technical_correctness: "Technical correctness",
  novelty: "Novelty",
  reproducibility: "Reproducibility",
  citation: "Citation",
  meta_reviewer: "Meta reviewer",
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
    <Accordion type="multiple" className="w-full">
      {ordered.map((agent) => (
        <AccordionItem key={agent.role} value={agent.role}>
          <AccordionTrigger>
            <div className="flex flex-1 items-center justify-between gap-3 pr-2">
              <div className="flex items-center gap-3">
                <span className="font-medium">
                  {ROLE_LABEL[agent.role] ?? agent.role}
                </span>
                <Badge variant="outline" className="font-mono text-xs">
                  {agent.model}
                </Badge>
              </div>
              <VerifierStatusBadge status={agent.verifier_status} />
            </div>
          </AccordionTrigger>
          <AccordionContent>
            <pre className="overflow-x-auto rounded-md bg-[color:var(--color-muted)] p-4 text-xs">
              {JSON.stringify(agent.output, null, 2)}
            </pre>
          </AccordionContent>
        </AccordionItem>
      ))}
    </Accordion>
  );
}
