import { NextResponse } from "next/server";
import { z } from "zod";
import { forwardToOrchestrator, requireServiceToken } from "../../../_lib";

const Body = z.object({
  format: z.enum(["html", "md", "tex", "pdf", "zip"]).default("html"),
});

export async function POST(
  req: Request,
  { params }: { params: Promise<{ id: string }> },
) {
  const unauth = requireServiceToken(req);
  if (unauth) return unauth;
  const { id } = await params;
  const parsed = Body.safeParse(await req.json().catch(() => ({})));
  if (!parsed.success) {
    return NextResponse.json(
      { error: "bad_body", detail: parsed.error.flatten() },
      { status: 400 },
    );
  }
  return forwardToOrchestrator(`/internal/v1/reviews/${id}/render`, {
    method: "POST",
    body: JSON.stringify(parsed.data),
  });
}
