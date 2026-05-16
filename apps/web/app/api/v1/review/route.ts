import { NextResponse } from "next/server";
import { z } from "zod";
import { forwardToOrchestrator, requireServiceToken } from "../_lib";

const Body = z.object({
  source: z.string().min(1),
  type: z.enum(["arxiv", "pdf", "tex", "mixed"]).optional(),
  mode: z.enum(["review_only", "review_and_revise"]).optional(),
  runner: z.enum(["api", "cli", "cloud", "local_inference"]).optional(),
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
  return forwardToOrchestrator("/internal/v1/review", {
    method: "POST",
    body: JSON.stringify(parsed.data),
  });
}
