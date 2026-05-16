import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { PUBLIC_REVIEW_STATUSES, type ReviewStatus } from "@/lib/types";
import { isSupabaseConfigured } from "@/lib/env";

const Query = z.object({
  page: z.coerce.number().int().min(1).max(10_000).default(1),
  limit: z.coerce.number().int().min(1).max(50).default(20),
  field: z.string().min(1).max(64).optional(),
  status: z.enum(PUBLIC_REVIEW_STATUSES as unknown as [ReviewStatus, ...ReviewStatus[]]).default("published"),
});

export async function GET(request: Request) {
  const url = new URL(request.url);
  const parsed = Query.safeParse(Object.fromEntries(url.searchParams));
  if (!parsed.success) {
    return NextResponse.json(
      { error: "bad_query", detail: parsed.error.flatten() },
      { status: 400 },
    );
  }
  const { page, limit, field, status } = parsed.data;

  if (!isSupabaseConfigured()) {
    return NextResponse.json({ data: [], page, total: 0 });
  }

  const supabase = await createSupabaseServerClient();
  const from = (page - 1) * limit;
  const to = from + limit - 1;
  let q = supabase
    .from("reviews")
    .select(
      "id, paper_id, status, github_pr_url, github_review_url, models_used, created_at, published_at, paper:papers!inner(field)",
      { count: "exact" },
    )
    .eq("status", status)
    .order("published_at", { ascending: false, nullsFirst: false })
    .range(from, to);
  if (field) q = q.eq("paper.field", field);

  const { data, count, error } = await q;
  if (error) {
    return NextResponse.json({ error: error.message }, { status: 500 });
  }
  // Strip the joined paper object from the row payload — the public list
  // returns ReviewSummary only.
  type Row = { paper?: unknown } & Record<string, unknown>;
  const cleaned = ((data ?? []) as Row[]).map((row) => {
    const { paper, ...rest } = row;
    void paper;
    return rest;
  });
  return NextResponse.json({ data: cleaned, page, total: count ?? cleaned.length });
}
