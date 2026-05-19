import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Public API",
  description:
    "Read-only JSON API for GrokRxiv reviews. CORS-open, no auth, paginated.",
};

const SAMPLE_LIST = `{
  "data": [
    {
      "id": "b1e6...",
      "paper_id": "f49a...",
      "status": "published",
      "github_pr_url": "https://github.com/GrokRxiv/grokrxiv-reviews/pull/42",
      "github_review_url": "https://github.com/GrokRxiv/grokrxiv-reviews/tree/main/reviews/2026/05/cs.LG/2401.12345",
      "created_at": "2026-05-13T18:00:00Z",
      "published_at": "2026-05-13T18:42:00Z"
    }
  ],
  "page": 1,
  "total": 142
}`;

function Endpoint({
  method,
  path,
  body,
  example,
}: {
  method: string;
  path: string;
  body: string;
  example?: string;
}) {
  return (
    <section className="flex min-w-0 flex-col gap-3 rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-3">
        <span className="rounded bg-[color:var(--color-secondary)] px-2 py-0.5 font-mono text-xs">
          {method}
        </span>
        <code className="break-all font-mono text-sm">{path}</code>
      </div>
      <p className="break-words text-sm text-[color:var(--color-muted-foreground)]">
        {body}
      </p>
      {example ? (
        <pre className="max-w-full overflow-x-auto rounded-md bg-[color:var(--color-muted)] p-3 text-xs">
          {example}
        </pre>
      ) : null}
    </section>
  );
}

export default function ApiDocsPage() {
  return (
    <div className="mx-auto flex w-full min-w-0 max-w-3xl flex-col gap-6">
      <header>
        <h1 className="text-3xl font-bold tracking-tight">Public JSON API</h1>
        <p className="mt-2 text-[color:var(--color-muted-foreground)]">
          Read-only, CORS-open, and paginated. Only public reviews are returned;
          moderation-pending, withdrawn, and private reviews are never exposed.
        </p>
      </header>

      <Endpoint
        method="GET"
        path="/api/v1/reviews"
        body="List published reviews. Query params: page (1+), limit (≤50), field, status (default published)."
        example={`curl https://grokrxiv.org/api/v1/reviews?limit=5\n\n${SAMPLE_LIST}`}
      />
      <Endpoint
        method="GET"
        path="/api/v1/reviews/:id"
        body="Fetch a single review by uuid. Returns 404 unless the review is public."
        example={`curl https://grokrxiv.org/api/v1/reviews/b1e6...`}
      />
      <Endpoint
        method="GET"
        path="/api/v1/papers/:source_id"
        body="Fetch a paper and all of its public reviews by arXiv id or GrokRxiv source id."
        example={`curl https://grokrxiv.org/api/v1/papers/2401.12345\ncurl https://grokrxiv.org/api/v1/papers/local-pdf-d96363843fd8`}
      />
      <Endpoint
        method="POST"
        path="/api/upload"
        body="Multipart upload. Returns a sample, single-pass review preview — never indexed as a real GrokRxiv review."
        example={`curl -F file=@paper.pdf https://grokrxiv.org/api/upload`}
      />

      <section className="rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4 sm:p-6">
        <h2 className="mb-2 text-xl font-semibold">Review metadata</h2>
        <p className="break-words text-sm text-[color:var(--color-muted-foreground)]">
          API responses include review status, source paper identifiers, archive
          links, and timestamps. Treat additional fields as optional metadata
          that may evolve over time.
        </p>
      </section>
    </div>
  );
}
