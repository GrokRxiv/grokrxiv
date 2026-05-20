import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { Suspense } from "react";
import { cacheTag } from "next/cache";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ReviewStatusBadge } from "@/components/review-status-badge";
import { MathText } from "@/components/math-text";
import {
  displayFieldForPaper,
  SourceBadge,
  SourceLink,
} from "@/components/source-label";
import { getPaperByArxivIdAnon } from "@/lib/supabase/anon";
import { PUBLIC_REVIEW_STATUSES } from "@/lib/types";

async function loadPaper(arxiv: string) {
  "use cache";
  cacheTag(`paper:${arxiv}`);
  cacheTag("reviews-list");
  return getPaperByArxivIdAnon(arxiv);
}

type Params = { arxiv: string };

export async function generateMetadata({
  params,
}: {
  params: Promise<Params>;
}): Promise<Metadata> {
  const { arxiv } = await params;
  const data = await loadPaper(arxiv);
  if (!data) return { title: "Paper not found" };
  return {
    title: data.paper.title,
    description: data.paper.abstract?.slice(0, 200),
  };
}

export default function PaperPage({
  params,
}: {
  params: Promise<Params>;
}) {
  return (
    <div className="flex flex-col gap-8">
      <Suspense fallback={<PaperSkeleton />}>
        <PaperBody params={params} />
      </Suspense>
    </div>
  );
}

async function PaperBody({ params }: { params: Promise<Params> }) {
  const { arxiv } = await params;
  const data = await loadPaper(arxiv);
  if (!data) notFound();
  const { paper, reviews } = data;
  const field = displayFieldForPaper(paper);
  const publicReviews = reviews.filter((r) =>
    PUBLIC_REVIEW_STATUSES.includes(r.status),
  );

  return (
    <>
      <header className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          {field ? <Badge variant="outline">{field}</Badge> : null}
          <SourceBadge paper={paper} />
        </div>
        <MathText
          as="h1"
          className="text-balance text-3xl font-bold tracking-tight md:text-4xl"
        >
          {paper.title}
        </MathText>
        <p className="text-sm text-[color:var(--color-muted-foreground)]">
          {paper.authors.map((a) => a.name).join(", ")}
        </p>
        <SourceLink
          paper={paper}
          className="break-all font-mono text-sm underline underline-offset-4"
        />
        {paper.abstract ? (
          <p className="max-w-3xl break-words text-[color:var(--color-muted-foreground)]">
            {paper.abstract}
          </p>
        ) : null}
      </header>

      <section className="flex flex-col gap-3">
        <h2 className="text-xl font-semibold">
          Reviews ({publicReviews.length})
        </h2>
        {publicReviews.length === 0 ? (
          <p className="text-sm text-[color:var(--color-muted-foreground)]">
            No published reviews yet for this paper.
          </p>
        ) : (
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            {publicReviews.map((r) => (
              <Link key={r.id} href={`/reviews/${r.id}`} className="min-w-0">
                <Card className="transition-shadow hover:shadow-md">
                  <CardHeader>
                    <div className="flex items-center justify-between gap-2">
                      <ReviewStatusBadge status={r.status} />
                      <span className="font-mono text-xs text-[color:var(--color-muted-foreground)]">
                        {new Date(r.published_at ?? r.created_at)
                          .toISOString()
                          .slice(0, 10)}
                      </span>
                    </div>
                    <CardTitle className="break-all text-base">
                      Review {r.id.slice(0, 8)}
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="break-words text-xs text-[color:var(--color-muted-foreground)]">
                    Open the full review for the summary, recommendation, and
                    detailed findings.
                  </CardContent>
                </Card>
              </Link>
            ))}
          </div>
        )}
      </section>
    </>
  );
}

function PaperSkeleton() {
  return (
    <div className="flex flex-col gap-4">
      <div className="h-8 w-32 animate-pulse rounded bg-[color:var(--color-muted)]" />
      <div className="h-10 w-3/4 animate-pulse rounded bg-[color:var(--color-muted)]" />
      <div className="h-32 w-full animate-pulse rounded bg-[color:var(--color-muted)]" />
    </div>
  );
}
