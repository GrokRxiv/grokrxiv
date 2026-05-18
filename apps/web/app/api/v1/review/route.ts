import { NextResponse } from "next/server";
import { z } from "zod";
import { forwardToOrchestrator, requireServiceToken } from "../_lib";

const Body = z.object({
  source: z.string().min(1),
  type: z.enum(["arxiv", "pdf", "tex", "mixed"]).optional(),
  mode: z.enum(["review_only", "review_and_revise"]).optional(),
  runner: z.enum(["api", "cli", "cloud", "local_inference"]).optional(),
  extractor: z.enum(["api", "cli"]).optional(),
  visibility: z.enum(["public", "private"]).default("public"),
  compute_profile: z
    .enum([
      "sample_preview",
      "public_free",
      "paid_standard",
      "paid_private",
      "premium_api",
    ])
    .default("public_free"),
  cost_cap_usd: z.number().positive().max(100).optional(),
  public_consent: z.boolean().optional(),
});

export async function POST(req: Request) {
  const unauth = requireServiceToken(req);
  if (unauth) return unauth;

  const raw = await req.json().catch(() => ({}));
  const parsed = Body.safeParse(raw);
  if (!parsed.success) {
    return NextResponse.json(
      { error: "bad_body", detail: parsed.error.flatten() },
      { status: 400 },
    );
  }
  const usesProviderApi =
    parsed.data.runner === "api" ||
    parsed.data.extractor === "api" ||
    parsed.data.compute_profile === "premium_api";
  if (
    usesProviderApi &&
    (parsed.data.compute_profile !== "premium_api" ||
      parsed.data.cost_cap_usd == null)
  ) {
    return NextResponse.json(
      {
        error: "premium_api_requires_cost_cap",
        detail:
          "Premium jobs must explicitly use compute_profile=premium_api and set cost_cap_usd.",
      },
      { status: 400 },
    );
  }
  if (parsed.data.visibility === "public" && parsed.data.public_consent === false) {
    return NextResponse.json(
      {
        error: "public_consent_required",
        detail:
          "Full public reviews require consent that approved or rejected output may become public.",
      },
      { status: 400 },
    );
  }
  return forwardToOrchestrator("/internal/v1/review", {
    method: "POST",
    body: JSON.stringify(parsed.data),
  });
}
