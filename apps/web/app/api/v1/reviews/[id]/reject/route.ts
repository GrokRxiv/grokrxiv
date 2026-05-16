import { NextResponse } from "next/server";
import { z } from "zod";
import { forwardToOrchestrator, requireServiceToken } from "../../../_lib";

const Body = z.object({
  reason: z.string().min(1).max(2000),
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
  return forwardToOrchestrator(`/internal/v1/reviews/${id}/reject`, {
    method: "POST",
    body: JSON.stringify(parsed.data),
  });
}
