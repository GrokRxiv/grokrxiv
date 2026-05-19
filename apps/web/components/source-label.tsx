import Link from "next/link";
import { Badge } from "@/components/ui/badge";
import type { Paper } from "@/lib/types";

type SourceInfo = {
  kind: string;
  label: string;
  detail: string;
  uri: string | null;
  hash: string | null;
  isArxiv: boolean;
};

const LOCAL_KIND_LABELS: Record<string, string> = {
  pdf: "PDF upload",
  local_pdf: "PDF upload",
  tex: "TeX source",
  local_tex: "TeX source",
  mixed: "Uploaded sources",
  git: "Git repository",
  github: "Git repository",
  git_repo: "Git repository",
  repository: "Git repository",
};

export function sourceInfoForPaper(paper: Paper): SourceInfo {
  const rawKind = paper.source_kind?.trim().toLowerCase() || "arxiv";
  const isArxiv = rawKind === "arxiv";
  const sourceId = paper.source_id?.trim() || (isArxiv ? paper.arxiv_id : "");
  const uri = publicSafeUri(paper.source_uri);
  const hash = publicSafeHash(paper.source_hash);

  if (isArxiv) {
    return {
      kind: rawKind,
      label: "arXiv",
      detail: sourceId ? `arXiv:${sourceId}` : "arXiv source",
      uri: sourceId ? `https://arxiv.org/abs/${sourceId}` : uri,
      hash,
      isArxiv: true,
    };
  }

  return {
    kind: rawKind,
    label: LOCAL_KIND_LABELS[rawKind] ?? humanizeSourceKind(rawKind),
    detail: sourceId || LOCAL_KIND_LABELS[rawKind] || humanizeSourceKind(rawKind),
    uri,
    hash,
    isArxiv: false,
  };
}

export function SourceBadge({ paper }: { paper: Paper }) {
  const source = sourceInfoForPaper(paper);
  return (
    <Badge variant="secondary" className="font-mono">
      {source.detail}
    </Badge>
  );
}

export function SourceLink({
  paper,
  className,
}: {
  paper: Paper;
  className?: string;
}) {
  const source = sourceInfoForPaper(paper);
  if (!source.uri) {
    return <span className={`${className ?? ""} no-underline`}>{source.detail}</span>;
  }
  return (
    <Link
      href={source.uri}
      target="_blank"
      rel="noopener noreferrer"
      className={className}
    >
      {source.detail}
    </Link>
  );
}

export function SourceDetails({ paper }: { paper: Paper }) {
  const source = sourceInfoForPaper(paper);
  const rows = [
    { label: "Source", value: source.label },
    source.hash ? { label: "Hash", value: source.hash } : null,
    source.uri ? { label: "URI", value: source.uri } : null,
  ].filter((row): row is { label: string; value: string } => row !== null);

  return (
    <dl className="grid gap-2 text-sm">
      {rows.map((row) => (
        <div key={row.label} className="grid gap-1">
          <dt className="text-xs uppercase tracking-wide text-[color:var(--color-muted-foreground)]">
            {row.label}
          </dt>
          <dd className="break-all font-mono">{row.value}</dd>
        </div>
      ))}
    </dl>
  );
}

export function sourceAriaLabel(paper: Paper): string {
  const source = sourceInfoForPaper(paper);
  return `Review of ${source.detail}`;
}

export function formatSubmissionSourceLabel(
  source: string,
  sourceType?: string | null,
): string {
  const kind = sourceType?.trim().toLowerCase() || "";
  const label = LOCAL_KIND_LABELS[kind] ?? (kind ? humanizeSourceKind(kind) : "");
  const value = source.trim();
  if (!value || isLocalPath(value)) return label || "Local source";
  return label ? `${label}: ${value}` : value;
}

function humanizeSourceKind(kind: string): string {
  return kind
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ") || "Paper source";
}

function publicSafeUri(uri: string | null | undefined): string | null {
  const value = uri?.trim();
  if (!value || isLocalPath(value)) return null;
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:"
      ? parsed.toString()
      : null;
  } catch {
    return null;
  }
}

function publicSafeHash(hash: string | null | undefined): string | null {
  const value = hash?.trim();
  if (!value || isLocalPath(value)) return null;
  return value.length > 24 ? `${value.slice(0, 24)}...` : value;
}

function isLocalPath(value: string): boolean {
  return (
    value.startsWith("/") ||
    value.startsWith("~/") ||
    value.startsWith("file:") ||
    /^[a-zA-Z]:[\\/]/.test(value)
  );
}
