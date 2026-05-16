import { Badge } from "@/components/ui/badge";
import type { ReviewStatus, VerifierStatus } from "@/lib/types";

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
};

const STATUS_LABEL: Record<ReviewStatus, string> = {
  draft: "Draft",
  awaiting_moderation: "Awaiting moderation",
  in_review: "In review",
  pr_open: "PR open",
  published: "Published",
  corrected: "Corrected",
  withdrawn: "Withdrawn",
};

export function ReviewStatusBadge({ status }: { status: ReviewStatus }) {
  return <Badge variant={STATUS_VARIANT[status]}>{STATUS_LABEL[status]}</Badge>;
}

const VERIFIER_VARIANT: Record<
  VerifierStatus,
  "success" | "warn" | "destructive"
> = {
  pass: "success",
  warn: "warn",
  fail: "destructive",
};

export function VerifierStatusBadge({ status }: { status: VerifierStatus }) {
  return (
    <Badge variant={VERIFIER_VARIANT[status]}>verifier: {status}</Badge>
  );
}
