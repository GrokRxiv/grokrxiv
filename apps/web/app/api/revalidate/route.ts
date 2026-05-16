import { NextResponse } from "next/server";
import { revalidatePath } from "next/cache";
import { headers } from "next/headers";
import { z } from "zod";
import { REVALIDATE_SECRET } from "@/lib/env";

const Body = z.object({
  review_id: z.string().uuid(),
  paths: z.array(z.string().min(1)).optional(),
});

export async function POST(request: Request) {
  const hdrs = await headers();
  const provided = hdrs.get("x-revalidate-secret");
  if (!REVALIDATE_SECRET || provided !== REVALIDATE_SECRET) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }

  let parsed;
  try {
    const json = (await request.json()) as unknown;
    parsed = Body.parse(json);
  } catch (err) {
    return NextResponse.json(
      { error: err instanceof Error ? err.message : "bad request" },
      { status: 400 },
    );
  }

  const revalidated: string[] = [];
  revalidatePath("/");
  revalidated.push("/");
  const reviewPath = `/reviews/${parsed.review_id}`;
  revalidatePath(reviewPath);
  revalidated.push(reviewPath);
  for (const extra of parsed.paths ?? []) {
    revalidatePath(extra);
    revalidated.push(extra);
  }
  return NextResponse.json({ ok: true, revalidated });
}
