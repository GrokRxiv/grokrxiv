import { NextResponse } from "next/server";
import { revalidatePath, updateTag } from "next/cache";
import { headers } from "next/headers";
import { z } from "zod";
import { REVALIDATE_SECRET } from "@/lib/env";

const Body = z.object({
  review_id: z.string().uuid(),
  paths: z.array(z.string().min(1)).optional(),
  /** Optional arxiv id to flush the matching paper page too. */
  arxiv_id: z.string().optional(),
});

export async function POST(request: Request) {
  const hdrs = await headers();
  const provided = hdrs.get("x-revalidate-secret");
  if (!REVALIDATE_SECRET || provided !== REVALIDATE_SECRET) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }

  let parsed;
  try {
    const json = (await request.json()) as unknown;
    parsed = Body.parse(json);
  } catch (err) {
    return NextResponse.json(
      { error: err instanceof Error ? err.message : "bad request" },
      { status: 400 },
    );
  }

  // Cache Components ('use cache' directive) are invalidated by tag, not by
  // path. revalidatePath only flushes ISR-style caches, which is why the home
  // grid (page.tsx's ReviewsGrid) stayed stale after we called it.
  //
  // Tag scheme — kept in sync with the cacheTag(...) calls in:
  //   - apps/web/app/page.tsx::ReviewsGrid               → reviews-list
  //   - apps/web/app/reviews/[id]/page.tsx::loadReviewWithPaper
  //                                                       → review:<uuid>, reviews-list
  //   - apps/web/app/papers/[arxiv]/page.tsx::loadPaper  → paper:<arxiv>, reviews-list
  const revalidated: string[] = [];
  // Next.js 16 Cache Components: updateTag(tag) invalidates every `'use
  // cache'` function tagged with `cacheTag(tag)`. revalidateTag exists but
  // requires a CacheLife profile; updateTag is the right invalidator.
  updateTag("reviews-list");
  revalidated.push("tag:reviews-list");
  updateTag(`review:${parsed.review_id}`);
  revalidated.push(`tag:review:${parsed.review_id}`);
  if (parsed.arxiv_id) {
    updateTag(`paper:${parsed.arxiv_id}`);
    revalidated.push(`tag:paper:${parsed.arxiv_id}`);
  }

  // Belt-and-suspenders: revalidatePath still flushes ISR / dynamic-route
  // caches that aren't cache-component-backed. Cheap; keep it.
  revalidatePath("/");
  revalidated.push("/");
  const reviewPath = `/reviews/${parsed.review_id}`;
  revalidatePath(reviewPath);
  revalidated.push(reviewPath);
  for (const extra of parsed.paths ?? []) {
    revalidatePath(extra);
    revalidated.push(extra);
  }
  return NextResponse.json({ ok: true, revalidated });
}
