import Link from "next/link";
import { Suspense } from "react";
import { cacheTag } from "next/cache";
import { Button } from "@/components/ui/button";
import { UploadDropzone } from "@/components/upload-dropzone";
import { PipelineDiagram } from "@/components/pipeline-diagram";
import { ReviewCard } from "@/components/review-card";
import { listPublishedReviewsAnon } from "@/lib/supabase/anon";

export default function HomePage() {
  return (
    <div className="flex flex-col gap-16 md:gap-24">
      {/* Hero */}
      <section className="flex flex-col items-center gap-6 pt-8 text-center md:pt-12">
        <h1 className="max-w-4xl text-balance break-words text-3xl font-bold tracking-tight sm:text-4xl md:text-6xl">
          <span className="font-mono text-[color:var(--color-foreground)]">
            GrokRxiv
          </span>
          <br />
          <span className="text-[color:var(--color-foreground)]">
            an agentic peer-review system that automates the
          </span>{" "}
          <span className="bg-gradient-to-r from-sky-400 via-fuchsia-400 to-amber-300 bg-clip-text text-transparent">
            review → revise → publish
          </span>{" "}
          <span>pipeline for arXiv papers.</span>
        </h1>
        <p className="max-w-2xl text-balance text-lg text-[color:var(--color-muted-foreground)]">
          Each report summarizes the paper, checks the core claims, reviews
          citations, and records the moderation outcome. Approved public
          reviews are archived at{" "}
          <Link
            href="https://github.com/GrokRxiv/grokrxiv-reviews"
            className="underline underline-offset-4"
          >
            github.com/GrokRxiv/grokrxiv-reviews
          </Link>
          .
        </p>
        <div className="flex items-center gap-3">
          <Button asChild size="lg">
            <Link href="#sample-review">Try sample review</Link>
          </Button>
          <Button asChild variant="outline" size="lg">
            <Link href="#how">How it works</Link>
          </Button>
        </div>
      </section>

      {/* Upload */}
      <section id="sample-review" className="flex scroll-mt-24 flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">
          Try a sample review
        </h2>
        <UploadDropzone />
      </section>

      {/* Latest reviews */}
      <section id="reviews" className="flex flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">
          Latest reviews
        </h2>
        <Suspense fallback={<ReviewsGridSkeleton />}>
          <ReviewsGrid />
        </Suspense>
      </section>

      {/* Pipeline */}
      <section id="how" className="flex flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">How it works</h2>
        <PipelineDiagram />
      </section>
    </div>
  );
}

async function ReviewsGrid() {
  "use cache";
  cacheTag("reviews-list");
  const { data, error } = await listPublishedReviewsAnon({ limit: 9 });
  if (error) {
    return (
      <div className="rounded-lg border border-amber-600 bg-amber-950/20 p-4 text-sm text-amber-100">
        <p className="font-medium">Review data setup needs attention.</p>
        <p className="mt-1 text-amber-100/80">{error}</p>
      </div>
    );
  }
  if (data.length === 0) {
    return (
      <p className="text-sm text-[color:var(--color-muted-foreground)]">
        No reviews yet. Be the first to drop a PDF above.
      </p>
    );
  }
  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
      {data.map((r) => (
        <ReviewCard key={r.id} review={r} />
      ))}
    </div>
  );
}

function ReviewsGridSkeleton() {
  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
      {Array.from({ length: 6 }).map((_, i) => (
        <div
          key={i}
          className="h-56 animate-pulse rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)]"
        />
      ))}
    </div>
  );
}
