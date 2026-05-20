import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { PUBLIC_REVIEW_STATUSES } from "@/lib/types";
import { isSupabaseConfigured } from "@/lib/env";

const SourceKeyParam = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9._-]+$/);

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ arxiv: string }> },
) {
  const { arxiv } = await params;
  const parsed = SourceKeyParam.safeParse(arxiv);
  if (!parsed.success) {
    return NextResponse.json({ error: "bad_source_id" }, { status: 400 });
  }
  if (!isSupabaseConfigured()) {
    return NextResponse.json({ error: "not_configured" }, { status: 503 });
  }
  const supabase = await createSupabaseServerClient();
  const { data: paper, error: paperErr } = await supabase
    .from("papers")
    .select("*")
    .or(`arxiv_id.eq.${parsed.data},source_id.eq.${parsed.data}`)
    .single();
  if (paperErr || !paper) {
    return NextResponse.json({ error: "not_found" }, { status: 404 });
  }
  const { data: reviews } = await supabase
    .from("reviews")
    .select(
      "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, created_at, published_at",
    )
    .eq("paper_id", (paper as { id: string }).id)
    .eq("visibility", "public")
    .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
    .order("published_at", { ascending: false, nullsFirst: false });
  return NextResponse.json({ paper, reviews: reviews ?? [] });
}
