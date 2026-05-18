// Anon Supabase client suitable for caching: no cookies, no per-request state.
// Use this in `'use cache'` functions, `generateMetadata`, and sitemap routes.

import { createClient } from "@supabase/supabase-js";
import {
  SUPABASE_ANON_KEY,
  SUPABASE_URL,
  isSupabaseConfigured,
} from "@/lib/env";
import {
  PUBLIC_REVIEW_STATUSES,
  type Paper,
  type Review,
  type ReviewSummary,
  type ReviewWithPaper,
} from "@/lib/types";

function client() {
  return createClient(SUPABASE_URL, SUPABASE_ANON_KEY, {
    auth: { persistSession: false, autoRefreshToken: false },
  });
}

export async function listPublishedReviewsAnon(opts: {
  limit?: number;
  page?: number;
  field?: string;
  /** Optional case-insensitive search over `papers.title` and `papers.abstract`. */
  q?: string;
}): Promise<{ data: ReviewWithPaper[]; total: number }> {
  if (!isSupabaseConfigured()) return { data: [], total: 0 };
  const supabase = client();
  const limit = opts.limit ?? 12;
  const page = opts.page ?? 1;
  const from = (page - 1) * limit;
  const to = from + limit - 1;
  // Filtering / searching on the joined paper table requires `!inner` so the
  // ILIKE / EQ runs server-side and the count is accurate.
  const wantsPaperFilter =
    (opts.field && opts.field.length > 0) || (opts.q && opts.q.length > 0);
  const paperSelect = wantsPaperFilter ? "paper:papers!inner(*)" : "paper:papers(*)";
  let qb = supabase
    .from("reviews")
    .select(
      `id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, meta_review, created_at, published_at, ${paperSelect}`,
      { count: "exact" },
    )
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    // Sort by created_at so pr_open rows (NULL published_at) surface
    // alongside published rows by recency, instead of being shoved to the end.
    .order("created_at", { ascending: false })
    .range(from, to);
  if (opts.field) qb = qb.eq("paper.field", opts.field);
  if (opts.q && opts.q.length > 0) {
    const needle = opts.q.replace(/[%_]/g, "\\$&");
    qb = qb.or(
      `title.ilike.%${needle}%,abstract.ilike.%${needle}%`,
      { foreignTable: "paper" },
    );
  }
  const { data, count, error } = await qb;
  if (error || !data) return { data: [], total: 0 };
  return {
    data: data as unknown as ReviewWithPaper[],
    total: count ?? data.length,
  };
}

export async function getReviewByIdAnon(id: string): Promise<Review | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = client();
  // Status filter MUST stay in the query — RLS is belt, this is suspenders.
  // Without it, a `withdrawn` or `awaiting_moderation` review fetched by id
  // would leak through to anon if RLS is ever misconfigured.
  const { data, error } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, meta_review, created_at, published_at, agents:review_agents(role, model, output, verifier_status)",
    )
    .eq("id", id)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .single();
  if (error || !data) return null;
  return data as unknown as Review;
}

/// Phase 4: fetch the moderator's rationale for a rejected review. Returns
/// null when the review is not rejected (RLS denies cross-status reads).
export async function getRejectionByReviewIdAnon(
  reviewId: string,
): Promise<{ rationale_md: string; created_at: string } | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = client();
  const { data, error } = await supabase
    .from("rejections")
    .select("rationale_md, created_at")
    .eq("review_id", reviewId)
    .order("created_at", { ascending: false })
    .limit(1)
    .maybeSingle();
  if (error || !data) return null;
  return data as { rationale_md: string; created_at: string };
}

// Paper rows are only exposed once at least one public-visibility review in a
// public status references them. Otherwise anon could enumerate the ingestion
// queue or private review corpus.
async function paperHasPublicReviewAnon(
  supabase: ReturnType<typeof client>,
  paperId: string,
): Promise<boolean> {
  const { count } = await supabase
    .from("reviews")
    .select("id", { count: "exact", head: true })
    .eq("paper_id", paperId)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[]);
  return (count ?? 0) > 0;
}

export async function getPaperByIdAnon(id: string): Promise<Paper | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = client();
  const { data, error } = await supabase
    .from("papers")
    .select("*")
    .eq("id", id)
    .single();
  if (error || !data) return null;
  if (!(await paperHasPublicReviewAnon(supabase, (data as Paper).id))) return null;
  return data as Paper;
}

export async function getPaperByArxivIdAnon(arxivId: string): Promise<{
  paper: Paper;
  reviews: ReviewSummary[];
} | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = client();
  const { data: paper, error } = await supabase
    .from("papers")
    .select("*")
    .eq("arxiv_id", arxivId)
    .single();
  if (error || !paper) return null;
  const { data: reviews } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, created_at, published_at",
    )
    .eq("paper_id", (paper as Paper).id)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .order("created_at", { ascending: false });
  // Withhold the paper itself if no public review exists.
  if (!reviews || reviews.length === 0) return null;
  return {
    paper: paper as Paper,
    reviews: reviews as ReviewSummary[],
  };
}

export async function listAllPublishedReviewIdsAnon(): Promise<
  { id: string; published_at: string | null }[]
> {
  if (!isSupabaseConfigured()) return [];
  const supabase = client();
  const { data } = await supabase
    .from("reviews")
    .select("id, published_at")
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[]);
  return (data ?? []) as { id: string; published_at: string | null }[];
}

export async function listAllPaperArxivIdsAnon(): Promise<
  { arxiv_id: string; ingested_at: string }[]
> {
  if (!isSupabaseConfigured()) return [];
  const supabase = client();
  // Only enumerate papers that have at least one publicly visible review.
  // Subquery via the `reviews` join in Supabase JS isn't expressive enough;
  // do it via a server-side filter on a view-like select.
  const { data } = await supabase
    .from("reviews")
    .select("paper:papers(arxiv_id, ingested_at)")
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[]);
  if (!data) return [];
  const seen = new Set<string>();
  const out: { arxiv_id: string; ingested_at: string }[] = [];
  for (const row of data as unknown as {
    paper: { arxiv_id: string; ingested_at: string } | null;
  }[]) {
    if (!row.paper || seen.has(row.paper.arxiv_id)) continue;
    seen.add(row.paper.arxiv_id);
    out.push(row.paper);
  }
  return out;
}
