import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { Suspense } from "react";
import { cacheTag } from "next/cache";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { AgentAccordion } from "@/components/agent-accordion";
import { ReviewStatusBadge } from "@/components/review-status-badge";
import { MarkdownBody } from "@/components/markdown-body";
import { MathText } from "@/components/math-text";
import { ReviewToc } from "@/components/review-toc";
import { JsonLd } from "@/components/json-ld";
import {
  getPaperByIdAnon,
  getReviewByIdAnon,
  getRejectionByReviewIdAnon,
} from "@/lib/supabase/anon";
import { CANONICAL_URL } from "@/lib/env";
import { buildTocFromMarkdown } from "@/lib/toc";
import {
  PUBLIC_REVIEW_STATUSES,
  type Paper,
  type Review,
} from "@/lib/types";

type Params = { id: string };

async function loadReviewWithPaper(
  id: string,
): Promise<{
  review: Review;
  paper: Paper;
  rejection: { rationale_md: string; created_at: string } | null;
} | null> {
  "use cache";
  cacheTag(`review:${id}`);
  cacheTag("reviews-list");
  const review = await getReviewByIdAnon(id);
  if (!review) return null;
  // Hide non-public statuses (e.g. awaiting_moderation, withdrawn) from anon
  // readers. Defense-in-depth — RLS should already deny.
  if (!PUBLIC_REVIEW_STATUSES.includes(review.status)) return null;
  const paper = await getPaperByIdAnon(review.paper_id);
  if (!paper) return null;
  const rejection =
    review.status === "rejected"
      ? await getRejectionByReviewIdAnon(id)
      : null;
  return { review, paper, rejection };
}

export async function generateMetadata({
  params,
}: {
  params: Promise<Params>;
}): Promise<Metadata> {
  const { id } = await params;
  const data = await loadReviewWithPaper(id);
  if (!data) {
    return { title: "Review not found" };
  }
  const { paper, review } = data;
  const title = `${paper.title} — GrokRxiv review of arXiv:${paper.arxiv_id}`;
  const summary = review.meta_review?.summary?.trim();
  const description = (() => {
    if (summary && summary.length > 0) {
      // Description should lead with the paper title so the snippet Google
      // shows starts with searchable content, not a generic intro.
      const tail = summary.slice(0, 180).replace(/\s+/g, " ").trim();
      return `GrokRxiv AI peer review of "${paper.title}" (arXiv:${paper.arxiv_id}). ${tail}`;
    }
    return `GrokRxiv AI peer review of "${paper.title}" (arXiv:${paper.arxiv_id}).`;
  })();
  const url = `${CANONICAL_URL}/reviews/${id}`;
  return {
    title,
    description,
    alternates: { canonical: url },
    openGraph: {
      type: "article",
      url,
      title,
      description,
    },
    twitter: {
      card: "summary_large_image",
      title,
      description,
    },
  };
}

export default function ReviewPage({
  params,
}: {
  params: Promise<Params>;
}) {
  return (
    <Suspense fallback={<ReviewSkeleton />}>
      <ReviewBody params={params} />
    </Suspense>
  );
}

async function ReviewBody({ params }: { params: Promise<Params> }) {
  const { id } = await params;
  const data = await loadReviewWithPaper(id);
  if (!data) notFound();
  const { review, paper, rejection } = data;

  const arxivUrl = `https://arxiv.org/abs/${paper.arxiv_id}`;
  const bibtex = buildBibtex(paper, review, id);

  const reviewUrl = `${CANONICAL_URL}/reviews/${id}`;
  const jsonLd = {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "ScholarlyArticle",
        "@id": arxivUrl,
        name: paper.title,
        headline: paper.title,
        identifier: `arXiv:${paper.arxiv_id}`,
        author: paper.authors.map((a) => ({
          "@type": "Person",
          name: a.name,
          ...(a.affiliation
            ? {
                affiliation: {
                  "@type": "Organization",
                  name: a.affiliation,
                },
              }
            : {}),
        })),
        sameAs: arxivUrl,
        url: arxivUrl,
      },
      {
        "@type": "Review",
        "@id": reviewUrl,
        url: reviewUrl,
        name: `GrokRxiv review of "${paper.title}"`,
        itemReviewed: { "@id": arxivUrl },
        author: {
          "@type": "Organization",
          name: "GrokRxiv",
          url: CANONICAL_URL,
        },
        publisher: {
          "@type": "Organization",
          name: "GrokRxiv",
          url: CANONICAL_URL,
        },
        reviewBody: review.meta_review?.summary ?? "",
        datePublished: review.published_at ?? review.created_at,
        inLanguage: "en",
        isAccessibleForFree: true,
      },
    ],
  };

  const metaReviewMd = buildMetaReviewMarkdown(review);
  const tocItems = [
    { id: "meta-review", text: "Overall review", level: 2 as const },
    ...buildTocFromMarkdown(metaReviewMd),
    { id: "review-details", text: "Review details", level: 2 as const },
  ];

  return (
    <>
      <JsonLd data={jsonLd} />
      <div className="grid grid-cols-1 gap-8 lg:grid-cols-[minmax(0,1fr)_280px]">
        <article className="flex min-w-0 flex-col gap-6">
          <header className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <ReviewStatusBadge status={review.status} />
              {paper.field ? (
                <Badge variant="outline">{paper.field}</Badge>
              ) : null}
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
            <div className="flex flex-wrap gap-3 text-sm">
              <Link
                href={arxivUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="break-all font-mono underline underline-offset-4"
              >
                arXiv:{paper.arxiv_id}
              </Link>
              {review.github_pr_url ? (
                <Link
                  href={review.github_pr_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="break-all underline underline-offset-4"
                >
                  Publication record
                </Link>
              ) : null}
              {review.github_review_url ? (
                <Link
                  href={review.github_review_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="break-all underline underline-offset-4"
                >
                  Review archive
                </Link>
              ) : null}
            </div>
          </header>

          {rejection ? (
            <section
              className="flex flex-col gap-3 rounded-md border border-red-700 bg-red-950/40 p-4"
              aria-label="Moderator rejection rationale"
            >
              <h2 className="text-lg font-semibold text-red-200">
                Moderator rejection rationale
              </h2>
              <div className="prose-review prose-invert text-red-50">
                <MarkdownBody>{rejection.rationale_md}</MarkdownBody>
              </div>
            </section>
          ) : null}

          <Separator />

          <section className="flex flex-col gap-4">
            <h2
              id="meta-review"
              className="text-2xl font-semibold border-b border-[color:var(--color-border)] pb-2"
            >
              Overall review
            </h2>
            <div className="prose-review">
              <MetaReviewBody markdown={metaReviewMd} />
            </div>
          </section>

          <Separator />

          <section className="flex flex-col gap-4">
            <h2
              id="review-details"
              className="text-2xl font-semibold border-b border-[color:var(--color-border)] pb-2"
            >
              Review details
            </h2>
            <AgentAccordion agents={review.agents ?? []} />
          </section>
        </article>

        <aside className="flex flex-col gap-6 lg:sticky lg:top-20 lg:self-start">
          <ReviewToc items={tocItems} />
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Downloads</CardTitle>
            </CardHeader>
            <CardContent className="flex flex-col gap-2">
              {review.github_review_url ? (
                <Button asChild variant="default" className="w-full">
                  <a
                    href={`${review.github_review_url}/bundle.zip`}
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    Download review package
                  </a>
                </Button>
              ) : null}
              {review.github_pr_url ? (
                <Button asChild variant="outline" className="w-full">
                  <a
                    href={review.github_pr_url}
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    Open publication record
                  </a>
                </Button>
              ) : null}
              <Button asChild variant="outline" className="w-full">
                <a href={arxivUrl} target="_blank" rel="noopener noreferrer">
                  View on arXiv
                </a>
              </Button>
              <details className="rounded-md border border-[color:var(--color-border)] p-3">
                <summary className="cursor-pointer text-sm font-medium">
                  Cite this review (BibTeX)
                </summary>
                <pre className="mt-2 max-w-full overflow-x-auto whitespace-pre-wrap break-all text-xs">{bibtex}</pre>
              </details>
            </CardContent>
          </Card>
        </aside>
      </div>
    </>
  );
}

function ReviewSkeleton() {
  return (
    <div className="flex flex-col gap-6">
      <div className="h-6 w-1/3 animate-pulse rounded bg-[color:var(--color-muted)]" />
      <div className="h-10 w-3/4 animate-pulse rounded bg-[color:var(--color-muted)]" />
      <div className="h-40 w-full animate-pulse rounded bg-[color:var(--color-muted)]" />
    </div>
  );
}

function buildMetaReviewMarkdown(review: Review): string {
  const mr = review.meta_review;
  if (!mr) return "No meta review yet.";
  return [
    mr.summary,
    "",
    "## Strengths",
    ...mr.strengths.map((s) => `- ${s}`),
    "",
    "## Weaknesses",
    ...mr.weaknesses.map((s) => `- ${s}`),
    "",
    "## Questions",
    ...mr.questions.map((s) => `- ${s}`),
    "",
    "## Recommendation",
    "",
    `**${mr.recommendation}** (confidence ${mr.confidence.toFixed(2)})`,
  ].join("\n");
}

function MetaReviewBody({ markdown }: { markdown: string }) {
  return <MarkdownBody>{markdown}</MarkdownBody>;
}

function buildBibtex(paper: Paper, review: Review, id: string): string {
  const year = new Date(
    review.published_at ?? review.created_at,
  ).getUTCFullYear();
  const key = `grokrxiv:${paper.arxiv_id.replace(/[^a-zA-Z0-9]/g, "")}`;
  const authors = paper.authors.map((a) => a.name).join(" and ");
  return `@misc{${key},
  title  = {{Review of "${paper.title.replace(/[{}]/g, "")}"}},
  author = {{GrokRxiv}},
  note   = {AI peer review of ${authors}, arXiv:${paper.arxiv_id}},
  year   = {${year}},
  url    = {${CANONICAL_URL}/reviews/${id}}
}`;
}
