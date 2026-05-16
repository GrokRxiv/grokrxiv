import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { PUBLIC_REVIEW_STATUSES } from "@/lib/types";
import { isSupabaseConfigured } from "@/lib/env";

const ArxivParam = z.string().regex(/^\d{4}\.\d{4,6}(v\d+)?$/);

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ arxiv: string }> },
) {
  const { arxiv } = await params;
  const parsed = ArxivParam.safeParse(arxiv);
  if (!parsed.success) {
    return NextResponse.json({ error: "bad_arxiv_id" }, { status: 400 });
  }
  if (!isSupabaseConfigured()) {
    return NextResponse.json({ error: "not_configured" }, { status: 503 });
  }
  const supabase = await createSupabaseServerClient();
  const { data: paper, error: paperErr } = await supabase
    .from("papers")
    .select("*")
    .eq("arxiv_id", parsed.data)
    .single();
  if (paperErr || !paper) {
    return NextResponse.json({ error: "not_found" }, { status: 404 });
  }
  const { data: reviews } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, github_pr_url, github_review_url, models_used, created_at, published_at",
    )
    .eq("paper_id", (paper as { id: string }).id)
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .order("published_at", { ascending: false, nullsFirst: false });
  return NextResponse.json({ paper, reviews: reviews ?? [] });
}
