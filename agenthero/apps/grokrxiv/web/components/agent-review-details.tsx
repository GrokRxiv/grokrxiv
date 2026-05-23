import { Badge } from "@/components/ui/badge";
import { RevisionTargetList } from "@/components/revision-target-card";
import type {
  AgentRole,
  CitationReferenceOutput,
  CitationReviewOutput,
  MetaReview,
  MissingReferenceOutput,
  NoveltyReviewOutput,
  ReproducibilityReviewOutput,
  RevisionTarget,
  RevisionTargetKind,
  RevisionTargetStatus,
  SummaryReviewOutput,
  TechnicalReviewOutput,
} from "@/lib/types";

export function AgentReviewDetails({
  role,
  output,
  verifierNotes,
}: {
  role: AgentRole;
  output: unknown;
  verifierNotes?: unknown | null;
}) {
  return (
    <div className="flex flex-col gap-4 pb-4">
      {renderRoleDetails(role, output, verifierNotes)}
      <details className="rounded-md border border-[color:var(--color-border)] bg-slate-950/35 p-3">
        <summary className="cursor-pointer text-xs font-medium uppercase tracking-wide text-slate-300">
          Debug JSON
        </summary>
        <pre className="mt-3 max-h-96 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950/70 p-3 text-xs text-slate-100">
          {JSON.stringify(output, null, 2)}
        </pre>
      </details>
      {verifierNotes ? (
        <details className="rounded-md border border-[color:var(--color-border)] bg-slate-950/35 p-3">
          <summary className="cursor-pointer text-xs font-medium uppercase tracking-wide text-slate-300">
            Verifier provenance
          </summary>
          <pre className="mt-3 max-h-96 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950/70 p-3 text-xs text-slate-100">
            {JSON.stringify(verifierNotes, null, 2)}
          </pre>
        </details>
      ) : null}
    </div>
  );
}

function renderRoleDetails(
  role: AgentRole,
  output: unknown,
  verifierNotes?: unknown | null,
) {
  switch (role) {
    case "summary":
      return <SummaryDetails review={parseSummary(output)} />;
    case "technical_correctness":
      return <TechnicalDetails review={parseTechnical(output)} />;
    case "novelty":
      return <NoveltyDetails review={parseNovelty(output)} />;
    case "reproducibility":
      return <ReproducibilityDetails review={parseReproducibility(output)} />;
    case "citation":
      return (
        <CitationDetails
          review={parseCitation(output)}
          verifierNotes={verifierNotes}
        />
      );
    case "meta_reviewer":
      return <MetaReviewerDetails review={parseMetaReviewer(output)} />;
    default:
      return <GenericAgentDetails output={output} />;
  }
}

function GenericAgentDetails({ output }: { output: unknown }) {
  const record = isRecord(output) ? output : null;
  const summary = record
    ? (stringField(record, "summary") ??
      stringField(record, "tldr") ??
      stringField(record, "verdict") ??
      null)
    : null;

  return (
    <div className="flex flex-col gap-4">
      <Field label="Summary" value={summary} />
    </div>
  );
}

function SummaryDetails({ review }: { review: SummaryReviewOutput }) {
  return (
    <div className="flex flex-col gap-4">
      <Field label="TLDR" value={review.tldr} strong />
      <Field
        label="Plain language summary"
        value={review.plain_language_summary}
      />
      <Field label="Audience" value={review.audience} />
      <ListBlock
        title="Key contributions"
        items={review.key_contributions}
        empty="No key contributions provided."
      />
    </div>
  );
}

function TechnicalDetails({ review }: { review: TechnicalReviewOutput }) {
  return (
    <div className="flex flex-col gap-4">
      <MetricGrid
        items={[
          ["Overall correctness", review.overall_correctness],
          ["Confidence", formatNumber(review.confidence)],
        ]}
      />
      <div className="flex flex-col gap-3">
        <SectionTitle>Claims</SectionTitle>
        {review.claims.length > 0 ? (
          review.claims.map((claim, index) => (
            <div
              key={claim.id ?? index}
              className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
            >
              <div className="mb-3 flex flex-wrap items-center gap-2">
                <span className="font-mono text-xs text-slate-400">
                  {claim.id ?? `claim-${index + 1}`}
                </span>
                <StatusBadge value={claim.assessment} />
                <StatusBadge value={claim.severity} />
              </div>
              <Field label="Claim" value={claim.claim} strong />
              <Field label="Location" value={claim.location} />
              <Field label="Evidence" value={claim.evidence} />
              <Field label="Suggested fix" value={claim.suggested_fix} />
            </div>
          ))
        ) : (
          <EmptyState>No claims provided.</EmptyState>
        )}
      </div>
    </div>
  );
}

function NoveltyDetails({ review }: { review: NoveltyReviewOutput }) {
  return (
    <div className="flex flex-col gap-4">
      <MetricGrid
        items={[
          ["Novelty score", formatNumber(review.novelty_score)],
          ["Verdict", review.verdict],
          ["Confidence", formatNumber(review.confidence)],
        ]}
      />
      <div className="flex flex-col gap-3">
        <SectionTitle>Related work</SectionTitle>
        {review.related_work.length > 0 ? (
          review.related_work.map((work, index) => (
            <div
              key={`${work.citation_key ?? work.title ?? "work"}-${index}`}
              className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
            >
              <div className="mb-2 flex flex-wrap items-center gap-2">
                {work.citation_key ? (
                  <span className="font-mono text-xs text-slate-400">
                    {work.citation_key}
                  </span>
                ) : null}
                <StatusBadge value={work.relation} />
              </div>
              <Field label="Title" value={work.title} strong />
              <Field label="Delta" value={work.delta} />
            </div>
          ))
        ) : (
          <EmptyState>No related work provided.</EmptyState>
        )}
      </div>
      <MissingReferenceList
        title="Missing prior art"
        items={review.missing_prior_art}
      />
    </div>
  );
}

function ReproducibilityDetails({
  review,
}: {
  review: ReproducibilityReviewOutput;
}) {
  return (
    <div className="flex flex-col gap-4">
      <MetricGrid
        items={[
          ["Code availability", review.code_availability],
          ["Code URL", review.code_url],
          ["Data availability", review.data_availability],
          ["Data URL", review.data_url],
          ["Score", formatNumber(review.reproducibility_score)],
          ["Confidence", formatNumber(review.confidence)],
        ]}
      />
      <div className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4">
        <SectionTitle>Environment</SectionTitle>
        {review.environment ? (
          <div className="mt-3 flex flex-col gap-3">
            <Field label="Hardware" value={review.environment.hardware} />
            <Field label="Software" value={review.environment.software} />
            <ListBlock
              title="Dependencies"
              items={review.environment.dependencies}
              empty="No dependencies provided."
            />
          </div>
        ) : (
          <EmptyState>No environment provided.</EmptyState>
        )}
      </div>
      <div className="flex flex-col gap-3">
        <SectionTitle>Concerns</SectionTitle>
        {review.concerns.length > 0 ? (
          review.concerns.map((concern, index) => (
            <div
              key={`${concern.area ?? "concern"}-${index}`}
              className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
            >
              <div className="mb-2 flex flex-wrap items-center gap-2">
                <StatusBadge value={concern.area} />
                <StatusBadge value={concern.severity} />
              </div>
              <Field label="Description" value={concern.description} />
            </div>
          ))
        ) : (
          <EmptyState>No concerns provided.</EmptyState>
        )}
      </div>
    </div>
  );
}

function CitationDetails({
  review,
  verifierNotes,
}: {
  review: CitationReviewOutput;
  verifierNotes?: unknown | null;
}) {
  const verifierEntries = citationVerifierEntries(verifierNotes);
  const verifierIndex = citationVerifierIndex(verifierEntries);
  const coverage = citationVerifierCoverage(verifierNotes);
  return (
    <div className="flex flex-col gap-4">
      <MetricGrid
        items={[
          ["Summary", review.summary],
          ["Confidence", formatNumber(review.confidence)],
          ["External citation checks", coverage.label],
        ]}
      />
      {coverage.reason ? <Field label="Citation coverage note" value={coverage.reason} /> : null}
      <MissingReferenceList
        title="Suggested missing prior art"
        items={review.missing_references}
      />
      <div className="flex flex-col gap-3">
        <SectionTitle>Entries</SectionTitle>
        {review.entries.length > 0 ? (
          review.entries.map((entry, index) => {
            const verifierEntry = citationVerifierEntryFor(entry, verifierIndex);
            return (
              <div
                key={`${entry.citation?.key ?? "citation"}-${index}`}
                className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
              >
                <div className="mb-3 flex flex-wrap items-center gap-2">
                  <StatusBadge
                    value={citationStatusLabel(entry, verifierEntry)}
                  />
                  <StatusBadge value={entry.relevance} />
                </div>
                <Field
                  label="Citation"
                  value={
                    verifierEntry?.title ??
                    formatCitation(entry.citation) ??
                    verifierEntry?.citation_key ??
                    null
                  }
                  strong
                />
                <Field
                  label="Verifier source"
                  value={verifierEntry?.source ?? null}
                />
                <Field
                  label="Verifier note"
                  value={readableCitationReason(verifierEntry?.reason)}
                />
                <Field
                  label="Resolved DOI"
                  value={verifierEntry?.resolved_doi ?? entry.resolved_doi}
                />
                <Field
                  label="Resolved URL"
                  value={verifierEntry?.resolved_url ?? entry.resolved_url}
                />
                <Field label="Notes" value={entry.notes} />
                <Field label="Explanation" value={entry.explanation} />
              </div>
            );
          })
        ) : verifierEntries.length > 0 ? (
          verifierEntries.map((entry, index) => (
            <div
              key={`${entry.citation_key ?? entry.raw ?? "verifier"}-${index}`}
              className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
            >
              <div className="mb-3 flex flex-wrap items-center gap-2">
                <StatusBadge
                  value={citationVerifierStatusLabel(entry)}
                />
              </div>
              <Field
                label="Citation"
                value={entry.title ?? entry.citation_key ?? entry.raw}
                strong
              />
              <Field label="Verifier source" value={entry.source} />
              <Field
                label="Verifier note"
                value={readableCitationReason(entry.reason)}
              />
              <Field label="Resolved DOI" value={entry.resolved_doi} />
              <Field label="Resolved URL" value={entry.resolved_url} />
            </div>
          ))
        ) : (
          <EmptyState>No citation entries provided.</EmptyState>
        )}
      </div>
    </div>
  );
}

type CitationVerifierEntry = {
  raw: string | null;
  status: string | null;
  resolved_doi: string | null;
  resolved_url: string | null;
  source: string | null;
  reason: string | null;
  title: string | null;
  citation_key: string | null;
  doi: string | null;
  arxiv_id: string | null;
  url: string | null;
};

type CitationVerifierCoverage = {
  checked: number | null;
  label: string;
  reason: string | null;
};

function citationVerifierCoverage(
  verifierNotes: unknown,
): CitationVerifierCoverage {
  const notes = citationVerifierNotes(verifierNotes);
  const checked = notes ? numberField(notes, "checked") : null;
  const coverageStatus = notes ? stringField(notes, "coverage_status") : null;
  const reason = notes ? stringField(notes, "reason") : null;
  if (checked === 0 || coverageStatus === "not_checked") {
    return {
      checked: checked ?? 0,
      label: "Not externally checked",
      reason:
        reason ??
        "No extracted bibliography entries were available for external citation verification.",
    };
  }
  if (checked !== null) {
    return {
      checked,
      label: `${checked} reference${checked === 1 ? "" : "s"} checked`,
      reason,
    };
  }
  return {
    checked: null,
    label: "Verifier provenance unavailable",
    reason,
  };
}

function citationVerifierEntries(
  verifierNotes: unknown,
): CitationVerifierEntry[] {
  const notes = citationVerifierNotes(verifierNotes);
  return recordArrayField(notes ?? {}, "entries").map((entry) => ({
    raw: stringField(entry, "raw"),
    status: stringField(entry, "status"),
    resolved_doi: stringField(entry, "resolved_doi"),
    resolved_url: stringField(entry, "resolved_url"),
    source: stringField(entry, "source"),
    reason: stringField(entry, "reason"),
    title: stringField(entry, "title"),
    citation_key: stringField(entry, "citation_key"),
    doi: stringField(entry, "doi"),
    arxiv_id: stringField(entry, "arxiv_id"),
    url: stringField(entry, "url"),
  }));
}

function citationVerifierNotes(
  verifierNotes: unknown,
): Record<string, unknown> | null {
  const root = asRecord(verifierNotes);
  const citation = recordField(root, "citation");
  return citation ? recordField(citation, "notes") : root;
}

type CitationVerifierIndex = Map<string, CitationVerifierEntry>;

function citationVerifierIndex(
  entries: CitationVerifierEntry[],
): CitationVerifierIndex {
  const index = new Map<string, CitationVerifierEntry>();
  for (const entry of entries) {
    addCitationVerifierKey(index, "key", entry.citation_key, entry);
    addCitationVerifierKey(index, "doi", normalizeDoi(entry.doi), entry);
    addCitationVerifierKey(index, "doi", normalizeDoi(entry.resolved_doi), entry);
    addCitationVerifierKey(index, "arxiv", normalizeToken(entry.arxiv_id), entry);
    addCitationVerifierKey(index, "url", normalizeToken(entry.url), entry);
    addCitationVerifierKey(index, "url", normalizeToken(entry.resolved_url), entry);
    addCitationVerifierKey(index, "title", normalizeTitle(entry.title), entry);
    addCitationVerifierKey(index, "raw", normalizeTitle(entry.raw), entry);
  }
  return index;
}

function citationVerifierEntryFor(
  entry: CitationReviewOutput["entries"][number],
  index: CitationVerifierIndex,
): CitationVerifierEntry | null {
  const citation = entry.citation;
  const candidates = [
    ["key", citation?.key],
    ["doi", normalizeDoi(citation?.doi)],
    ["doi", normalizeDoi(entry.resolved_doi)],
    ["arxiv", normalizeToken(citation?.arxiv_id)],
    ["url", normalizeToken(citation?.url)],
    ["url", normalizeToken(entry.resolved_url)],
    ["title", normalizeTitle(citation?.title)],
    ["raw", normalizeTitle(citation?.raw)],
  ] as const;
  for (const [kind, value] of candidates) {
    if (!value) continue;
    const match = index.get(`${kind}:${value}`);
    if (match) return match;
  }
  return null;
}

function addCitationVerifierKey(
  index: CitationVerifierIndex,
  kind: string,
  value: string | null,
  entry: CitationVerifierEntry,
) {
  if (!value) return;
  const key = `${kind}:${value}`;
  if (!index.has(key)) index.set(key, entry);
}

function normalizeDoi(value?: string | null): string | null {
  const normalized = normalizeToken(value)
    ?.replace(/^https?:\/\/(dx\.)?doi\.org\//, "")
    .replace(/^doi:/, "");
  return normalized || null;
}

function normalizeTitle(value?: string | null): string | null {
  return value?.toLowerCase().replace(/\\s+/g, " ").trim() || null;
}

function normalizeToken(value?: string | null): string | null {
  return value?.toLowerCase().trim() || null;
}

function citationStatusLabel(
  entry: CitationReviewOutput["entries"][number],
  verifierEntry: CitationVerifierEntry | null,
): string | null {
  if (verifierEntry?.resolved_doi || verifierEntry?.resolved_url) {
    return "verified";
  }
  switch (verifierEntry?.status) {
    case "resolved":
      return "verified";
    case "unresolved":
      return "not resolved";
    case "unverified":
      return "needs review";
    case "transient_unknown":
      return "temporarily unknown";
    case "malformed":
      return "malformed";
  }
  if (entry.exists === true) return "verified";
  if (entry.exists === false) return "not resolved";
  return "unverified";
}

function citationVerifierStatusLabel(
  verifierEntry: CitationVerifierEntry,
): string | null {
  return citationStatusLabel(
    {
      citation: null,
      exists: null,
      resolved_doi: null,
      resolved_url: null,
      relevance: "medium",
      notes: null,
      explanation: "",
    },
    verifierEntry,
  );
}

function readableCitationReason(reason?: string | null): string | null {
  if (!reason) return null;
  const lower = reason.toLowerCase();
  if (
    lower.includes("no bibliographic match above score threshold") ||
    lower.includes("no match above score threshold")
  ) {
    return "Crossref bibliographic search did not find a strong match. This needs human review; it is not proof that the reference is fake.";
  }
  if (lower.includes("crossref status 404") && lower.includes("doi resolver status 404")) {
    return "The DOI was not found by Crossref or the DOI resolver.";
  }
  if (lower.includes("crossref status 404")) {
    return "Crossref does not have a matching record.";
  }
  if (lower.includes("doi resolver status 404")) {
    return "The DOI resolver returned 404.";
  }
  if (lower.includes("not present in arxiv response")) {
    return "The arXiv API did not return this identifier.";
  }
  return reason;
}

function MetaReviewerDetails({ review }: { review: MetaReview }) {
  return (
    <div className="flex flex-col gap-4">
      <MetricGrid
        items={[
          ["Recommendation", review.recommendation],
          ["Confidence", formatNumber(review.confidence)],
        ]}
      />
      <Field label="Summary" value={review.summary} />
      <ListBlock
        title="Strengths"
        items={review.strengths}
        empty="No strengths provided."
      />
      <ListBlock
        title="Weaknesses"
        items={review.weaknesses}
        empty="No weaknesses provided."
      />
      <div className="flex flex-col gap-2">
        <SectionTitle>Revision Targets</SectionTitle>
        <RevisionTargetList
          targets={review.revision_targets ?? []}
          compact
        />
      </div>
      <ListBlock
        title="Questions"
        items={review.questions}
        empty="No questions provided."
      />
    </div>
  );
}

function MetricGrid({
  items,
}: {
  items: Array<[label: string, value: string | number | null]>;
}) {
  return (
    <dl className="grid gap-3 sm:grid-cols-2">
      {items.map(([label, value]) => (
        <div
          key={label}
          className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-3"
        >
          <dt className="text-xs font-medium uppercase tracking-wide text-slate-400">
            {label}
          </dt>
          <dd className="mt-1 break-words text-sm text-slate-100">
            {displayValue(value)}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function Field({
  label,
  value,
  strong = false,
}: {
  label: string;
  value: string | number | null;
  strong?: boolean;
}) {
  return (
    <div className="flex flex-col gap-1">
      <div className="text-xs font-medium uppercase tracking-wide text-slate-400">
        {label}
      </div>
      <div
        className={
          strong
            ? "whitespace-pre-wrap break-words text-sm font-medium text-slate-50"
            : "whitespace-pre-wrap break-words text-sm text-slate-200"
        }
      >
        {displayValue(value)}
      </div>
    </div>
  );
}

function ListBlock({
  title,
  items,
  empty,
}: {
  title: string;
  items: string[];
  empty: string;
}) {
  return (
    <div className="flex flex-col gap-2">
      <SectionTitle>{title}</SectionTitle>
      {items.length > 0 ? (
        <ul className="list-disc space-y-2 pl-5 text-sm text-slate-200">
          {items.map((item, index) => (
            <li
              key={`${item}-${index}`}
              className="whitespace-pre-wrap break-words"
            >
              {item}
            </li>
          ))}
        </ul>
      ) : (
        <EmptyState>{empty}</EmptyState>
      )}
    </div>
  );
}

function MissingReferenceList({
  title,
  items,
}: {
  title: string;
  items: MissingReferenceOutput[];
}) {
  return (
    <div className="flex flex-col gap-3">
      <SectionTitle>{title}</SectionTitle>
      {items.length > 0 ? (
        items.map((item, index) => (
          <div
            key={`${item.title ?? "missing"}-${index}`}
            className="rounded-md border border-[color:var(--color-border)] bg-slate-950/25 p-4"
          >
            <Field label="Title" value={item.title} strong />
            <Field label="Reason" value={item.reason} />
          </div>
        ))
      ) : (
        <EmptyState>No entries provided.</EmptyState>
      )}
    </div>
  );
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return <h3 className="text-sm font-semibold text-slate-100">{children}</h3>;
}

function StatusBadge({ value }: { value: string | null }) {
  if (!value) return null;
  return (
    <Badge variant="outline" className="border-slate-600 text-slate-200">
      {humanize(value)}
    </Badge>
  );
}

function EmptyState({ children }: { children: React.ReactNode }) {
  return <p className="text-sm text-slate-400">{children}</p>;
}

function parseSummary(value: unknown): SummaryReviewOutput {
  const record = asRecord(value);
  return {
    tldr: stringField(record, "tldr"),
    plain_language_summary: stringField(record, "plain_language_summary"),
    audience: stringField(record, "audience"),
    key_contributions: stringArrayField(record, "key_contributions"),
  };
}

function parseTechnical(value: unknown): TechnicalReviewOutput {
  const record = asRecord(value);
  return {
    claims: recordArrayField(record, "claims").map((claim) => ({
      id: stringField(claim, "id"),
      claim: stringField(claim, "claim"),
      assessment: stringField(claim, "assessment"),
      severity: stringField(claim, "severity"),
      location: stringField(claim, "location"),
      evidence: stringField(claim, "evidence"),
      suggested_fix: stringField(claim, "suggested_fix"),
    })),
    overall_correctness: stringField(record, "overall_correctness"),
    confidence: numberField(record, "confidence"),
  };
}

function parseNovelty(value: unknown): NoveltyReviewOutput {
  const record = asRecord(value);
  return {
    novelty_score: numberField(record, "novelty_score"),
    verdict: stringField(record, "verdict"),
    confidence: numberField(record, "confidence"),
    related_work: recordArrayField(record, "related_work").map((work) => ({
      citation_key: stringField(work, "citation_key"),
      title: stringField(work, "title"),
      relation: stringField(work, "relation"),
      delta: stringField(work, "delta"),
    })),
    missing_prior_art: recordArrayField(record, "missing_prior_art").map(
      parseMissingReference,
    ),
  };
}

function parseReproducibility(value: unknown): ReproducibilityReviewOutput {
  const record = asRecord(value);
  const environment = recordField(record, "environment");
  return {
    code_availability: stringField(record, "code_availability"),
    code_url: stringField(record, "code_url"),
    data_availability: stringField(record, "data_availability"),
    data_url: stringField(record, "data_url"),
    environment: environment
      ? {
          hardware: stringField(environment, "hardware"),
          software: stringField(environment, "software"),
          dependencies: stringArrayField(environment, "dependencies"),
        }
      : null,
    concerns: recordArrayField(record, "concerns").map((concern) => ({
      area: stringField(concern, "area"),
      description: stringField(concern, "description"),
      severity: stringField(concern, "severity"),
    })),
    reproducibility_score: numberField(record, "reproducibility_score"),
    confidence: numberField(record, "confidence"),
  };
}

function parseCitation(value: unknown): CitationReviewOutput {
  const record = asRecord(value);
  return {
    entries: recordArrayField(record, "entries").map((entry) => ({
      citation: parseCitationReference(recordField(entry, "citation")),
      exists: booleanField(entry, "exists"),
      resolved_doi: stringField(entry, "resolved_doi"),
      resolved_url: stringField(entry, "resolved_url"),
      relevance: stringField(entry, "relevance"),
      notes: stringField(entry, "notes"),
      explanation: stringField(entry, "explanation"),
    })),
    missing_references: recordArrayField(record, "missing_references").map(
      parseMissingReference,
    ),
    summary: stringField(record, "summary"),
    confidence: numberField(record, "confidence"),
  };
}

function parseMetaReviewer(value: unknown): MetaReview {
  const record = asRecord(value);
  return {
    summary: stringField(record, "summary") ?? "",
    strengths: stringArrayField(record, "strengths"),
    weaknesses: stringArrayField(record, "weaknesses"),
    questions: stringArrayField(record, "questions"),
    revision_targets: recordArrayField(record, "revision_targets").map(
      parseRevisionTarget,
    ),
    recommendation: metaRecommendation(record),
    confidence: numberField(record, "confidence") ?? 0,
  };
}

function parseRevisionTarget(record: Record<string, unknown>): RevisionTarget {
  return {
    id: stringField(record, "id") ?? "",
    weakness_index: numberField(record, "weakness_index") ?? 0,
    source_role: agentRoleField(record, "source_role"),
    target_kind: revisionTargetKind(record),
    source_path: stringField(record, "source_path"),
    locator: stringField(record, "locator"),
    evidence: stringField(record, "evidence"),
    required_update: stringField(record, "required_update") ?? "",
    verification_check: stringField(record, "verification_check") ?? "",
    status: revisionTargetStatus(record),
  };
}

function parseMissingReference(
  record: Record<string, unknown>,
): MissingReferenceOutput {
  return {
    title: stringField(record, "title") ?? stringField(record, "work"),
    reason: stringField(record, "reason") ?? stringField(record, "comment"),
  };
}

function parseCitationReference(
  record: Record<string, unknown> | null,
): CitationReferenceOutput | null {
  if (!record) return null;
  return {
    key: stringField(record, "key"),
    raw: stringField(record, "raw"),
    title: stringField(record, "title"),
    authors: stringArrayField(record, "authors"),
    year: numberField(record, "year"),
    venue: stringField(record, "venue"),
    doi: stringField(record, "doi"),
    arxiv_id: stringField(record, "arxiv_id"),
    url: stringField(record, "url"),
  };
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {};
}

function recordField(
  record: Record<string, unknown>,
  key: string,
): Record<string, unknown> | null {
  const value = record[key];
  return isRecord(value) ? value : null;
}

function stringField(
  record: Record<string, unknown>,
  key: string,
): string | null {
  const value = record[key];
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function numberField(
  record: Record<string, unknown>,
  key: string,
): number | null {
  const value = record[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function booleanField(
  record: Record<string, unknown>,
  key: string,
): boolean | null {
  const value = record[key];
  return typeof value === "boolean" ? value : null;
}

function stringArrayField(
  record: Record<string, unknown>,
  key: string,
): string[] {
  const value = record[key];
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === "string");
}

function recordArrayField(
  record: Record<string, unknown>,
  key: string,
): Array<Record<string, unknown>> {
  const value = record[key];
  if (!Array.isArray(value)) return [];
  return value.filter(isRecord);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function metaRecommendation(
  record: Record<string, unknown>,
): MetaReview["recommendation"] {
  const value = stringField(record, "recommendation");
  if (
    value === "accept" ||
    value === "minor_revision" ||
    value === "major_revision" ||
    value === "reject"
  ) {
    return value;
  }
  return "major_revision";
}

function agentRoleField(
  record: Record<string, unknown>,
  key: string,
): string | null {
  return stringField(record, key);
}

function revisionTargetKind(
  record: Record<string, unknown>,
): RevisionTargetKind {
  const value = stringField(record, "target_kind");
  if (
    value === "paper_tex" ||
    value === "paper_pdf" ||
    value === "code" ||
    value === "data" ||
    value === "bibliography" ||
    value === "review_text" ||
    value === "unknown"
  ) {
    return value;
  }
  return "unknown";
}

function revisionTargetStatus(
  record: Record<string, unknown>,
): RevisionTargetStatus {
  const value = stringField(record, "status");
  if (
    value === "open" ||
    value === "addressed" ||
    value === "still_open" ||
    value === "superseded" ||
    value === "unknown"
  ) {
    return value;
  }
  return "unknown";
}

function formatNumber(value: number | null): string | null {
  return value === null ? null : value.toFixed(2);
}

function formatCitation(
  citation: CitationReferenceOutput | null,
): string | null {
  if (!citation) return null;
  const title = citation.title ?? citation.raw;
  const prefix = citation.key ? `[${citation.key}] ` : "";
  const authors =
    citation.authors.length > 0 ? citation.authors.join(", ") : null;
  const details = [authors, citation.year, citation.venue]
    .filter((item) => item !== null)
    .join(" - ");
  return [prefix + (title ?? "Untitled citation"), details || null]
    .filter((item) => item !== null)
    .join("\n");
}

function displayValue(value: string | number | null): string {
  if (value === null || value === "") return "Not provided";
  return String(value);
}

function humanize(value: string): string {
  return value.replace(/_/g, " ");
}
