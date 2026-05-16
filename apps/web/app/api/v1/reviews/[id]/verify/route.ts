import { forwardToOrchestrator, requireServiceToken } from "../../../_lib";

export async function POST(
  req: Request,
  { params }: { params: Promise<{ id: string }> },
) {
  const unauth = requireServiceToken(req);
  if (unauth) return unauth;
  const { id } = await params;
  const body = await req.json().catch(() => ({}));
  return forwardToOrchestrator(`/internal/v1/reviews/${id}/verify`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}
