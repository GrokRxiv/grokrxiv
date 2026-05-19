import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { PUBLIC_REVIEW_STATUSES } from "@/lib/types";
import { isSupabaseConfigured } from "@/lib/env";

const UuidParam = z.string().uuid();
const SourceKeyParam = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9._-]+$/);

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

  const asSourceKey = SourceKeyParam.safeParse(id);
  if (asSourceKey.success) {
    const { data: paper } = await supabase
      .from("papers")
      .select("id")
      .or(`arxiv_id.eq.${asSourceKey.data},source_id.eq.${asSourceKey.data}`)
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

  return NextResponse.json({ error: "bad_source_id" }, { status: 400 });
}
