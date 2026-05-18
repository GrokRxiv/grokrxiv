import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { PUBLIC_REVIEW_STATUSES } from "@/lib/types";
import { isSupabaseConfigured } from "@/lib/env";

const UuidParam = z.string().uuid();
const ArxivParam = z.string().regex(/^\d{4}\.\d{4,6}(v\d+)?$/);

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ id: string }> },
) {
  const { id } = await params;
  if (!isSupabaseConfigured()) {
    return NextResponse.json({ error: "not_configured" }, { status: 503 });
  }
  const supabase = await createSupabaseServerClient();

  const select =
    "id, paper_id, status, visibility, github_pr_url, github_review_url, models_used, meta_review, created_at, published_at, paper:papers(*), agents:review_agents(role, model, output, verifier_status)";

  const asUuid = UuidParam.safeParse(id);
  if (asUuid.success) {
    const { data, error } = await supabase
      .from("reviews")
      .select(select)
      .eq("id", asUuid.data)
      .eq("visibility", "public")
      .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
      .single();
    if (error || !data) {
      return NextResponse.json({ error: "not_found" }, { status: 404 });
    }
    return NextResponse.json(data);
  }

  const asArxiv = ArxivParam.safeParse(id);
  if (asArxiv.success) {
    const { data: paper } = await supabase
      .from("papers")
      .select("id")
      .eq("arxiv_id", asArxiv.data)
      .single();
    if (!paper) {
      return NextResponse.json({ error: "not_found" }, { status: 404 });
    }
    const { data, error } = await supabase
      .from("reviews")
      .select(select)
      .eq("paper_id", (paper as { id: string }).id)
      .eq("visibility", "public")
      .in("status", PUBLIC_REVIEW_STATUSES as unknown as string[])
      .order("published_at", { ascending: false, nullsFirst: false })
      .limit(1)
      .single();
    if (error || !data) {
      return NextResponse.json({ error: "not_found" }, { status: 404 });
    }
    return NextResponse.json(data);
  }

  return NextResponse.json({ error: "bad_id" }, { status: 400 });
}
