import type { Metadata } from "next";
import Link from "next/link";
import { ReviewCard } from "@/components/review-card";
import { Button } from "@/components/ui/button";
import { listPublishedReviewsAnon } from "@/lib/supabase/anon";
import { CANONICAL_URL } from "@/lib/env";

// Common arXiv fields surfaced as filter chips. Hardcoded for v1; if/when a
// real `/api/v1/fields` endpoint lands the dropdown can switch to live data.
const COMMON_FIELDS = [
  "cs.AI",
  "cs.CL",
  "cs.LG",
  "cs.CV",
  "cs.CR",
  "math.AG",
  "math.CO",
  "math.NT",
  "stat.ML",
  "hep-th",
  "astro-ph",
  "cond-mat",
  "q-bio",
] as const;

const PAGE_SIZE = 20;
const TITLE = "GrokRxiv Reviews";
const DESCRIPTION =
  "Browse every published GrokRxiv AI peer review. Filter by arXiv field, search by title or abstract, paginate by date.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: `${CANONICAL_URL}/reviews` },
  openGraph: {
    title: TITLE,
    description: DESCRIPTION,
    type: "website",
    url: `${CANONICAL_URL}/reviews`,
    images: [{ url: `${CANONICAL_URL}/og.png`, alt: TITLE }],
  },
  twitter: {
    card: "summary_large_image",
    title: TITLE,
    description: DESCRIPTION,
  },
};

type SearchParams = {
  page?: string;
  q?: string;
  field?: string;
};

function clampPage(input: string | undefined): number {
  const n = Number.parseInt(input ?? "1", 10);
  if (!Number.isFinite(n) || n < 1) return 1;
  if (n > 10_000) return 10_000;
  return n;
}

function trimParam(input: string | undefined, max = 128): string | undefined {
  if (!input) return undefined;
  const t = input.trim().slice(0, max);
  return t.length > 0 ? t : undefined;
}

export default async function ReviewsIndexPage({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  const { page: pageRaw, q: qRaw, field: fieldRaw } = await searchParams;
  const page = clampPage(pageRaw);
  const q = trimParam(qRaw);
  const field = trimParam(fieldRaw, 32);

  const { data, total } = await listPublishedReviewsAnon({
    limit: PAGE_SIZE,
    page,
    field,
    q,
  });

  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));
  const hasPrev = page > 1;
  const hasNext = page < pageCount;

  const baseHref = (overrides: Partial<SearchParams> = {}) => {
    const params = new URLSearchParams();
    const next = { page: String(page), q, field, ...overrides };
    if (next.q) params.set("q", next.q);
    if (next.field) params.set("field", next.field);
    if (next.page && next.page !== "1") params.set("page", next.page);
    const qs = params.toString();
    return qs ? `/reviews?${qs}` : "/reviews";
  };

  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-8 py-10">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Reviews index
        </p>
        <h1 className="text-balance text-3xl font-bold tracking-tight md:text-4xl">
          {TITLE}
        </h1>
        <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
          Every review here is gated through a typed verifier ladder and merged
          by a human moderator on{" "}
          <Link
            href="https://github.com/GrokRxiv/grokrxiv-reviews"
            className="underline underline-offset-4"
          >
            github.com/GrokRxiv/grokrxiv-reviews
          </Link>{" "}
          before becoming visible. Newest first.
        </p>
      </header>

      <form
        method="get"
        action="/reviews"
        className="flex flex-col gap-3 rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4 sm:flex-row sm:items-end"
      >
        <label className="flex flex-1 flex-col gap-1 text-sm">
          <span className="font-medium">Search</span>
          <input
            type="search"
            name="q"
            defaultValue={q ?? ""}
            placeholder="title or abstract…"
            maxLength={128}
            className="rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-background)] px-3 py-2"
          />
        </label>
        <label className="flex flex-col gap-1 text-sm sm:w-48">
          <span className="font-medium">Field</span>
          <select
            name="field"
            defaultValue={field ?? ""}
            className="rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-background)] px-3 py-2"
          >
            <option value="">All fields</option>
            {COMMON_FIELDS.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </label>
        <Button type="submit" className="sm:w-32">
          Apply
        </Button>
      </form>

      <section className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-3 text-sm text-[color:var(--color-muted-foreground)]">
          <span>
            {total === 0
              ? "No matching reviews."
              : `Showing ${(page - 1) * PAGE_SIZE + 1}–${Math.min(
                  page * PAGE_SIZE,
                  total,
                )} of ${total}`}
          </span>
          <span>
            Page {page} of {pageCount}
          </span>
        </div>

        {data.length === 0 ? (
          <div className="rounded-lg border border-dashed border-[color:var(--color-border)] p-8 text-center text-sm text-[color:var(--color-muted-foreground)]">
            <p className="mb-2 font-medium">
              No reviews match your filters yet.
            </p>
            <p>
              Try widening the search, removing the field filter, or{" "}
              <Link href="/#upload" className="underline underline-offset-4">
                upload a sample PDF
              </Link>{" "}
              from the homepage.
            </p>
          </div>
        ) : (
          <ul className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
            {data.map((r) => (
              <li key={r.id}>
                <article aria-label={`Review of arXiv:${r.paper?.arxiv_id ?? ""}`}>
                  <ReviewCard review={r} />
                </article>
              </li>
            ))}
          </ul>
        )}

        <nav
          aria-label="Pagination"
          className="flex items-center justify-between gap-3 pt-2"
        >
          <Button asChild variant="outline" disabled={!hasPrev}>
            <Link
              href={
                hasPrev ? baseHref({ page: String(page - 1) }) : "/reviews"
              }
              aria-disabled={!hasPrev}
              aria-label="Previous page"
              tabIndex={hasPrev ? 0 : -1}
            >
              ← Prev
            </Link>
          </Button>
          <span className="text-sm text-[color:var(--color-muted-foreground)]">
            Page {page} of {pageCount}
          </span>
          <Button asChild variant="outline" disabled={!hasNext}>
            <Link
              href={
                hasNext ? baseHref({ page: String(page + 1) }) : "/reviews"
              }
              aria-disabled={!hasNext}
              aria-label="Next page"
              tabIndex={hasNext ? 0 : -1}
            >
              Next →
            </Link>
          </Button>
        </nav>
      </section>
    </div>
  );
}
