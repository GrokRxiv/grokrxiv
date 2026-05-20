import { CheckCircle2, AlertTriangle, XCircle } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import type { Recommendation, ReviewStatus, VerifierStatus } from "@/lib/types";

const STATUS_VARIANT: Record<
  ReviewStatus,
  "default" | "secondary" | "destructive" | "outline" | "success" | "warn"
> = {
  draft: "outline",
  awaiting_moderation: "warn",
  in_review: "secondary",
  pr_open: "warn",
  published: "success",
  corrected: "success",
  withdrawn: "destructive",
  rejected: "destructive",
};

const STATUS_LABEL: Record<ReviewStatus, string> = {
  draft: "Draft",
  awaiting_moderation: "Awaiting moderation",
  in_review: "In review",
  pr_open: "In Review",
  published: "Published",
  corrected: "Corrected",
  withdrawn: "Withdrawn",
  rejected: "Rejected",
};

export function ReviewStatusBadge({ status }: { status: ReviewStatus }) {
  return <Badge variant={STATUS_VARIANT[status]}>{STATUS_LABEL[status]}</Badge>;
}

const GATE_VARIANT: Record<
  Recommendation,
  "success" | "warn" | "destructive"
> = {
  accept: "success",
  minor_revision: "warn",
  major_revision: "warn",
  reject: "destructive",
};

const GATE_LABEL: Record<Recommendation, string> = {
  accept: "Gate passed",
  minor_revision: "Needs revision",
  major_revision: "Needs revision",
  reject: "Gate rejected",
};

export function AutomatedGateBadge({
  recommendation,
}: {
  recommendation?: Recommendation | null;
}) {
  if (!recommendation) return null;
  return (
    <Badge variant={GATE_VARIANT[recommendation]}>
      {GATE_LABEL[recommendation]}
    </Badge>
  );
}

const VERIFIER_VARIANT: Record<
  VerifierStatus,
  "success" | "warn" | "destructive"
> = {
  pass: "success",
  warn: "warn",
  fail: "destructive",
};

const VERIFIER_ICON: Record<VerifierStatus, React.ReactNode> = {
  pass: <CheckCircle2 className="h-3.5 w-3.5" aria-hidden="true" />,
  warn: <AlertTriangle className="h-3.5 w-3.5" aria-hidden="true" />,
  fail: <XCircle className="h-3.5 w-3.5" aria-hidden="true" />,
};

const VERIFIER_LABEL: Record<VerifierStatus, string> = {
  pass: "Pass",
  warn: "Warn",
  fail: "Fail",
};

export function VerifierStatusBadge({ status }: { status: VerifierStatus }) {
  return (
    <Badge
      variant={VERIFIER_VARIANT[status]}
      className="inline-flex items-center gap-1.5"
      aria-label={`Verifier: ${VERIFIER_LABEL[status]}`}
    >
      {VERIFIER_ICON[status]}
      <span>{VERIFIER_LABEL[status]}</span>
    </Badge>
  );
}
