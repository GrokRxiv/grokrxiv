import { Badge } from "@/components/ui/badge";
import { MathText } from "@/components/math-text";
import { cn } from "@/lib/utils";
import type {
  RevisionTarget,
  RevisionTargetKind,
  RevisionTargetStatus,
} from "@/lib/types";

export function RevisionTargetList({
  targets,
  empty = "No revision targets provided.",
  compact = false,
}: {
  targets: RevisionTarget[];
  empty?: string;
  compact?: boolean;
}) {
  if (targets.length === 0) {
    return (
      <p className="text-sm text-[color:var(--color-muted-foreground)]">
        {empty}
      </p>
    );
  }

  return (
    <div className={cn("grid gap-3", compact ? "gap-3" : "gap-4")}>
      {targets.map((target, index) => (
        <RevisionTargetCard
          key={target.id || `${target.target_kind}-${target.locator}-${index}`}
          target={target}
          compact={compact}
        />
      ))}
    </div>
  );
}

function RevisionTargetCard({
  target,
  compact,
}: {
  target: RevisionTarget;
  compact: boolean;
}) {
  const location = targetLocation(target);
  return (
    <article
      className={cn(
        "rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-card)]",
        compact ? "p-3" : "p-4 md:p-5",
      )}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <Badge
              variant="outline"
              className={cn(
                "border-[color:var(--color-border)]",
                statusTone(target.status),
              )}
            >
              {targetStatusLabel(target.status)}
            </Badge>
            <Badge
              variant="outline"
              className="border-[color:var(--color-border)] text-[color:var(--color-muted-foreground)]"
            >
              {targetKindLabel(target.target_kind)}
            </Badge>
          </div>
          <MathText
            as="h3"
            className={cn(
              "break-words font-semibold leading-snug text-[color:var(--color-foreground)]",
              compact ? "text-sm" : "text-base",
            )}
          >
            {targetHeading(target)}
          </MathText>
        </div>
      </div>

      <dl
        className={cn(
          "mt-4 grid gap-3",
          compact ? "text-sm" : "text-sm md:text-base",
        )}
      >
        <RevisionTargetField
          label="Location"
          value={location}
          tone="locator"
        />
        {target.evidence && target.evidence !== target.required_update ? (
          <RevisionTargetField
            label="Evidence"
            value={target.evidence}
            tone="evidence"
          />
        ) : null}
        <RevisionTargetField
          label="Required change"
          value={target.required_update}
          tone="required"
        />
        {target.verification_check ? (
          <RevisionTargetField
            label="Verification"
            value={target.verification_check}
            tone="verification"
          />
        ) : null}
      </dl>
    </article>
  );
}

function RevisionTargetField({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone: "locator" | "evidence" | "required" | "verification";
}) {
  return (
    <div
      className={cn(
        "grid gap-2 rounded-md border p-3 sm:grid-cols-[10rem_minmax(0,1fr)]",
        tone === "required"
          ? "border-amber-500/50 bg-amber-950/25"
          : "border-[color:var(--color-border)] bg-[color:var(--color-muted)]",
      )}
    >
      <dt
        className={cn(
          "text-xs font-semibold uppercase text-[color:var(--color-muted-foreground)]",
          tone === "required" && "text-amber-700 dark:text-amber-200",
        )}
      >
        {label}
      </dt>
      <dd
        className={cn(
          "min-w-0 whitespace-pre-wrap break-words leading-7 text-[color:var(--color-foreground)]",
          tone === "locator" &&
            "font-mono text-[0.9rem] leading-6 text-[color:var(--color-foreground)]",
          tone === "verification" &&
            "text-[color:var(--color-muted-foreground)]",
          tone === "required" &&
            "font-medium text-[color:var(--color-foreground)]",
        )}
      >
        <MathText as="span">{value}</MathText>
      </dd>
    </div>
  );
}

function targetHeading(target: RevisionTarget): string {
  const locator = target.locator ?? "";
  switch (target.target_kind) {
    case "data":
      return locator.includes("data availability")
        ? "Data availability and restricted inputs"
        : `Data target: ${shortText(locator || target.required_update, 80)}`;
    case "code":
      if (locator.includes("compute")) return "Compute reproducibility";
      if (locator.includes("configuration")) return "Experiment configuration";
      if (locator.includes("evaluation")) return "Evaluation pipeline";
      if (locator.includes("entrypoints")) return "Code release and entrypoints";
      if (locator.includes("SAC hyperparameters")) {
        return "SAC hyperparameters and reward scaling";
      }
      return locator
        ? `Code/reproducibility target: ${shortText(locator, 80)}`
        : "Code/reproducibility artifacts";
    case "bibliography":
      return `Bibliography: ${shortText(locator || target.required_update, 96)}`;
    case "paper_tex":
    case "paper_pdf":
      return `Manuscript: ${shortText(locator || target.required_update, 96)}`;
    case "review_text":
      return "Review text correction";
    default:
      return `Revision target: ${shortText(target.required_update, 96)}`;
  }
}

function targetLocation(target: RevisionTarget): string {
  if (target.source_path && target.locator) {
    return `${target.source_path} at ${target.locator}`;
  }
  if (target.source_path) return target.source_path;
  if (target.locator && target.target_kind === "data") {
    return `data/reproducibility artifacts: ${target.locator}`;
  }
  if (target.locator && target.target_kind === "code") {
    return `code/reproducibility artifacts: ${target.locator}`;
  }
  if (target.locator && target.target_kind === "bibliography") {
    return `bibliography entry: ${shortText(target.locator, 120)}`;
  }
  if (target.locator) return target.locator;
  if (target.target_kind === "data") return "data/reproducibility artifacts";
  if (target.target_kind === "code") return "code/reproducibility artifacts";
  if (target.target_kind === "bibliography") return "bibliography";
  return "review artifact";
}

function targetKindLabel(kind: RevisionTargetKind): string {
  switch (kind) {
    case "paper_tex":
      return "Manuscript";
    case "paper_pdf":
      return "PDF";
    case "code":
      return "Code";
    case "data":
      return "Data";
    case "bibliography":
      return "Bibliography";
    case "review_text":
      return "Review text";
    default:
      return "Target";
  }
}

function targetStatusLabel(status: RevisionTargetStatus): string {
  switch (status) {
    case "open":
      return "Open";
    case "addressed":
      return "Addressed";
    case "still_open":
      return "Still open";
    case "superseded":
      return "Superseded";
    default:
      return "Unknown";
  }
}

function statusTone(status: RevisionTargetStatus): string {
  switch (status) {
    case "addressed":
      return "border-emerald-500/60 text-emerald-700 dark:text-emerald-200";
    case "still_open":
    case "open":
      return "border-amber-500/60 text-amber-700 dark:text-amber-200";
    case "superseded":
      return "border-[color:var(--color-border)] text-[color:var(--color-muted-foreground)]";
    default:
      return "border-[color:var(--color-border)] text-[color:var(--color-muted-foreground)]";
  }
}

function shortText(text: string, maxChars: number): string {
  return text.length <= maxChars ? text : `${text.slice(0, maxChars - 3)}...`;
}
