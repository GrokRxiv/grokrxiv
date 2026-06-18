import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { Suspense, type ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { AgentAccordion } from "@/components/agent-accordion";
import {
  AutomatedGateBadge,
  ReviewStatusBadge,
} from "@/components/review-status-badge";
import { MarkdownBody } from "@/components/markdown-body";
import { MathText } from "@/components/math-text";
import { ReviewToc } from "@/components/review-toc";
import { RevisionTargetList } from "@/components/revision-target-card";
import { JsonLd } from "@/components/json-ld";
import {
  displayFieldForPaper,
  SourceDetails,
  SourceLink,
  sourceInfoForPaper,
} from "@/components/source-label";
import {
  getPaperByIdAnon,
  getReviewByIdAnon,
  getRejectionByReviewIdAnon,
} from "@/lib/supabase/anon";
import { CANONICAL_URL } from "@/lib/env";
import {
  PUBLIC_REVIEW_STATUSES,
  type MetaReview,
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
  const source = sourceInfoForPaper(paper);
  const title = `${paper.title} — GrokRxiv review of ${source.detail}`;
  const summary = review.meta_review?.summary?.trim();
  const description = (() => {
    if (summary && summary.length > 0) {
      // Description should lead with the paper title so the snippet Google
      // shows starts with searchable content, not a generic intro.
      const tail = summary.slice(0, 180).replace(/\s+/g, " ").trim();
      return `GrokRxiv AI peer review of "${paper.title}" (${source.detail}). ${tail}`;
    }
    return `GrokRxiv AI peer review of "${paper.title}" (${source.detail}).`;
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

  const source = sourceInfoForPaper(paper);
  const field = displayFieldForPaper(paper);
  const feedbackCommentUrl =
    review.gate_failure_comment_url ?? review.github_comment_url ?? null;
  const bibtex = buildBibtex(paper, review, id);
  const feedbackRevisionTargets = revisionTargetsForDisplay(review.meta_review);
  const gateInstructionsMd = stripRevisionTargetsSection(
    review.gate_failure_instructions ?? "",
  );

  const reviewUrl = `${CANONICAL_URL}/reviews/${id}`;
  const articleId = source.uri ?? reviewUrl;
  const jsonLd = {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "ScholarlyArticle",
        "@id": articleId,
        name: paper.title,
        headline: paper.title,
        identifier: source.detail,
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
        ...(source.uri ? { sameAs: source.uri, url: source.uri } : {}),
      },
      {
        "@type": "Review",
        "@id": reviewUrl,
        url: reviewUrl,
        name: `GrokRxiv review of "${paper.title}"`,
        itemReviewed: { "@id": articleId },
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

  const tocItems = buildReviewToc(review.meta_review);

  return (
    <>
      <JsonLd data={jsonLd} />
      <div className="grid grid-cols-1 gap-8 lg:grid-cols-[minmax(0,1fr)_280px]">
        <article className="flex min-w-0 flex-col gap-6">
          <header className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <ReviewStatusBadge status={review.status} />
              <AutomatedGateBadge
                status={review.status}
                recommendation={review.meta_review?.recommendation}
              />
              {field ? (
                <Badge variant="outline">{field}</Badge>
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
              <SourceLink
                paper={paper}
                className="break-all font-mono underline underline-offset-4"
              />
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
              {review.github_comment_url ? (
                <Link
                  href={review.github_comment_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="break-all underline underline-offset-4"
                >
                  Review comment
                </Link>
              ) : null}
            </div>
          </header>

          {rejection || hasGateFailure(review) ? (
            <section
              className="flex flex-col gap-3 rounded-md border border-red-700 bg-red-950/40 p-4"
              aria-label="Gate failure and rejection feedback"
            >
              <h2 className="text-lg font-semibold text-red-200">
                Review feedback
              </h2>
              {review.gate_failure_reason ? (
                <p className="text-sm text-red-50">
                  {review.gate_failure_reason}
                </p>
              ) : null}
              {feedbackRevisionTargets.length > 0 ? (
                <div className="flex flex-col gap-3">
                  <h3 className="text-base font-semibold text-red-50">
                    Targeted revisions
                  </h3>
                  <RevisionTargetList
                    targets={feedbackRevisionTargets}
                    compact
                  />
                </div>
              ) : null}
              {gateInstructionsMd ? (
                <div className="prose-review prose-invert text-red-50">
                  <MarkdownBody>{gateInstructionsMd}</MarkdownBody>
                </div>
              ) : null}
              {rejection ? (
                <div className="prose-review prose-invert text-red-50">
                  <MarkdownBody>{rejection.rationale_md}</MarkdownBody>
                </div>
              ) : null}
              {feedbackCommentUrl ? (
                <Link
                  href={feedbackCommentUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="break-all text-sm font-medium text-red-100 underline underline-offset-4"
                >
                  View GitHub feedback comment
                </Link>
              ) : null}
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
            <MetaReviewBody review={review.meta_review} />
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
              {source.uri ? (
                <Button asChild variant="outline" className="w-full">
                  <a href={source.uri} target="_blank" rel="noopener noreferrer">
                    {source.isArxiv ? "View on arXiv" : `Open ${source.label}`}
                  </a>
                </Button>
              ) : null}
              <SourceDetails paper={paper} />
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

function buildReviewToc(metaReview: MetaReview) {
  return [
    { id: "meta-review", text: "Overall review", level: 2 as const },
    { id: "strengths", text: "Strengths", level: 3 as const },
    { id: "weaknesses", text: "Weaknesses", level: 3 as const },
    ...(metaReview.revision_targets?.length
      ? [{ id: "revision-targets", text: "Revision targets", level: 3 as const }]
      : []),
    { id: "questions", text: "Questions", level: 3 as const },
    { id: "recommendation", text: "Recommendation", level: 3 as const },
    { id: "review-details", text: "Review details", level: 2 as const },
  ];
}

function MetaReviewBody({ review }: { review: MetaReview }) {
  const summaryParagraphs = readableParagraphs(review.summary);
  const revisionTargets = revisionTargetsForDisplay(review);

  return (
    <div className="flex flex-col gap-8">
      <div className="max-w-[76ch] rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4 md:p-5">
        <div className="space-y-4 text-base leading-8 text-[color:var(--color-foreground)] md:text-[1.05rem] md:leading-9">
          {summaryParagraphs.map((paragraph, index) => (
            <MathText as="p" key={`${paragraph.slice(0, 32)}-${index}`}>
              {paragraph}
            </MathText>
          ))}
        </div>
      </div>

      <MetaReviewSection id="strengths" title="Strengths">
        <ReviewList items={review.strengths} empty="No strengths provided." />
      </MetaReviewSection>

      <MetaReviewSection id="weaknesses" title="Weaknesses">
        <ReviewList items={review.weaknesses} empty="No weaknesses provided." />
      </MetaReviewSection>

      {revisionTargets.length > 0 ? (
        <MetaReviewSection id="revision-targets" title="Revision targets">
          <RevisionTargetList targets={revisionTargets} />
        </MetaReviewSection>
      ) : null}

      <MetaReviewSection id="questions" title="Questions">
        <ReviewList
          items={review.questions}
          empty="No open questions provided."
        />
      </MetaReviewSection>

      <MetaReviewSection id="recommendation" title="Recommendation">
        <div className="flex flex-wrap items-center gap-3 rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4">
          <Badge
            variant="outline"
            className="border-[color:var(--color-border)] text-[color:var(--color-foreground)]"
          >
            {review.recommendation}
          </Badge>
          <span className="text-sm text-[color:var(--color-muted-foreground)]">
            Confidence {review.confidence.toFixed(2)}
          </span>
        </div>
      </MetaReviewSection>
    </div>
  );
}

function revisionTargetsForDisplay(review: MetaReview) {
  return (review.revision_targets ?? []).filter((target) =>
    target.required_update.trim(),
  );
}

function stripRevisionTargetsSection(markdown: string): string {
  const lines = markdown.split("\n");
  const start = lines.findIndex((line) =>
    /^##\s+Targeted Revisions\s*$/i.test(line.trim()),
  );
  if (start < 0) return markdown.trim();

  const end = lines.findIndex(
    (line, index) => index > start && /^##\s+/.test(line.trim()),
  );
  return [...lines.slice(0, start), ...(end < 0 ? [] : lines.slice(end))]
    .join("\n")
    .trim();
}

function MetaReviewSection({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <section id={id} className="flex scroll-mt-20 flex-col gap-3">
      <h3 className="text-lg font-semibold text-[color:var(--color-foreground)]">
        {title}
      </h3>
      {children}
    </section>
  );
}

function ReviewList({ items, empty }: { items: string[]; empty: string }) {
  if (items.length === 0) {
    return (
      <p className="text-sm text-[color:var(--color-muted-foreground)]">
        {empty}
      </p>
    );
  }

  return (
    <ul className="grid gap-3">
      {items.map((item, index) => (
        <li
          key={`${item.slice(0, 32)}-${index}`}
          className="rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-4 text-sm leading-7 text-[color:var(--color-foreground)] md:text-base"
        >
          <MathText as="span">{item}</MathText>
        </li>
      ))}
    </ul>
  );
}

function readableParagraphs(text: string): string[] {
  const paragraphs = text
    .split(/\n{2,}/)
    .map((part) => part.replace(/\s+/g, " ").trim())
    .filter(Boolean);
  return paragraphs.flatMap(splitLongParagraph);
}

function splitLongParagraph(paragraph: string): string[] {
  const maxChars = 520;
  if (paragraph.length <= maxChars) return [paragraph];

  const chunks: string[] = [];
  const sentences = paragraph.split(/(?<=[.!?])\s+(?=[A-Z0-9])/);
  let current = "";
  for (const sentence of sentences) {
    const next = current ? `${current} ${sentence}` : sentence;
    if (next.length > maxChars && current) {
      chunks.push(current);
      current = sentence;
    } else {
      current = next;
    }
  }
  if (current) chunks.push(current);
  return chunks;
}

function hasGateFailure(review: Review): boolean {
  return Boolean(
    review.gate_failure_reason ||
      review.gate_failure_instructions ||
      review.gate_failure_comment_url ||
      review.github_comment_url,
  );
}

function buildBibtex(paper: Paper, review: Review, id: string): string {
  const year = new Date(
    review.published_at ?? review.created_at,
  ).getUTCFullYear();
  const source = sourceInfoForPaper(paper);
  const keySource = (paper.source_id || paper.arxiv_id || id).replace(
    /[^a-zA-Z0-9]/g,
    "",
  );
  const key = `grokrxiv:${keySource}`;
  const authors = paper.authors.map((a) => a.name).join(" and ");
  return `@misc{${key},
  title  = {{Review of "${paper.title.replace(/[{}]/g, "")}"}},
  author = {{GrokRxiv}},
  note   = {AI peer review of ${authors}, ${source.detail}},
  year   = {${year}},
  url    = {${CANONICAL_URL}/reviews/${id}}
}`;
}
