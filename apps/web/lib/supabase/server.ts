import { cookies } from "next/headers";
import { createServerClient } from "@supabase/ssr";
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
import { SUPABASE_AUTH_COOKIE_NAME } from "@/lib/supabase/cookie";
import { supabaseErrorMessage } from "@/lib/supabase/errors";

export async function createSupabaseServerClient() {
  const cookieStore = await cookies();
  return createServerClient(SUPABASE_URL, SUPABASE_ANON_KEY, {
    cookieOptions: { name: SUPABASE_AUTH_COOKIE_NAME },
    cookies: {
      getAll() {
        return cookieStore.getAll();
      },
      setAll(cookiesToSet) {
        try {
          for (const { name, value, options } of cookiesToSet) {
            cookieStore.set(name, value, options);
          }
        } catch {
          // Server Components cannot set cookies; safe to ignore.
        }
      },
    },
  });
}

// --- Read helpers used by Server Components ---

export async function listPublishedReviews(opts: {
  limit?: number;
  page?: number;
  field?: string;
}): Promise<{ data: ReviewWithPaper[]; total: number; error?: string }> {
  if (!isSupabaseConfigured()) {
    return { data: [], total: 0 };
  }
  const supabase = await createSupabaseServerClient();
  const limit = opts.limit ?? 12;
  const page = opts.page ?? 1;
  const from = (page - 1) * limit;
  const to = from + limit - 1;

  let query = supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, meta_review, created_at, published_at, paper:papers(*)",
      { count: "exact" },
    )
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    // Sort by created_at so pr_open rows (NULL published_at) surface
    // alongside published rows by recency, instead of being shoved to the end.
    .order("created_at", { ascending: false })
    .range(from, to);

  if (opts.field) {
    query = query.eq("papers.field", opts.field);
  }

  const { data, count, error } = await query;
  if (error || !data) {
    return {
      data: [],
      total: 0,
      error: error ? supabaseErrorMessage(error) : "Supabase query failed.",
    };
  }
  return {
    data: data as unknown as ReviewWithPaper[],
    total: count ?? data.length,
  };
}

export async function getReviewById(id: string): Promise<Review | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = await createSupabaseServerClient();
  const { data, error } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, meta_review, created_at, published_at, paper:papers(*), agents:review_agents(role, model, output, verifier_status, verifier_notes)",
    )
    .eq("id", id)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .single();
  if (error || !data) return null;
  return data as unknown as Review;
}

export async function getPaperByArxivId(arxivId: string): Promise<{
  paper: Paper;
  reviews: ReviewSummary[];
} | null> {
  return getPaperBySourceKey(arxivId);
}

export async function getPaperBySourceKey(sourceKey: string): Promise<{
  paper: Paper;
  reviews: ReviewSummary[];
} | null> {
  if (!isSupabaseConfigured()) return null;
  const supabase = await createSupabaseServerClient();
  const { data: paper, error: paperErr } = await supabase
    .from("papers")
    .select("*")
    .or(`arxiv_id.eq.${sourceKey},source_id.eq.${sourceKey}`)
    .single();
  if (paperErr || !paper) return null;
  const { data: reviews } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, created_at, published_at",
    )
    .eq("paper_id", (paper as Paper).id)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .order("created_at", { ascending: false });
  return {
    paper: paper as Paper,
    reviews: (reviews ?? []) as ReviewSummary[],
  };
}

export async function listAllPublishedReviewIds(): Promise<
  { id: string; published_at: string | null }[]
> {
  if (!isSupabaseConfigured()) return [];
  const supabase = await createSupabaseServerClient();
  const { data } = await supabase
    .from("reviews")
    .select("id, published_at")
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[]);
  return (data ?? []) as { id: string; published_at: string | null }[];
}

export async function listAllPaperArxivIds(): Promise<
  { arxiv_id: string; ingested_at: string }[]
> {
  if (!isSupabaseConfigured()) return [];
  const supabase = await createSupabaseServerClient();
  const { data } = await supabase
    .from("papers")
    .select("arxiv_id, ingested_at, reviews!inner(status)")
    .eq("reviews.visibility", "public")
    .in("reviews.status", PUBLIC_REVIEW_STATUSES as unknown as string[]);
  return (data ?? []).map(({ arxiv_id, ingested_at }) => ({
    arxiv_id,
    ingested_at,
  })) as { arxiv_id: string; ingested_at: string }[];
}
