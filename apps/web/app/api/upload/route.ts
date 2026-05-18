import { NextResponse } from "next/server";
import { headers } from "next/headers";
import { ORCHESTRATOR_INTERNAL_URL, SITE_URL } from "@/lib/env";

export const maxDuration = 60;

const MAX_BYTES = 20 * 1024 * 1024;

function badRequest(message: string, status = 400) {
  return NextResponse.json({ error: message }, { status });
}

export async function POST(request: Request) {
  // Same-origin enforcement. We do not advertise CORS for /api/upload.
  const hdrs = await headers();
  const origin = hdrs.get("origin");
  const allowedOrigin = new URL(SITE_URL).origin;
  if (origin && origin !== allowedOrigin) {
    return badRequest("Cross-origin uploads are not allowed.", 403);
  }

  let form: FormData;
  try {
    form = await request.formData();
  } catch {
    return badRequest("Expected multipart/form-data.");
  }

  const file = form.get("file");
  if (!(file instanceof File)) return badRequest("Missing file field.");
  if (file.type !== "application/pdf")
    return badRequest("Only application/pdf is accepted.");
  if (file.size <= 0) return badRequest("File is empty.");
  if (file.size > MAX_BYTES) return badRequest("File exceeds 20 MB.", 413);

  const upstream = new FormData();
  upstream.append("file", file, file.name);

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 60_000);

  try {
    const resp = await fetch(`${ORCHESTRATOR_INTERNAL_URL}/preview`, {
      method: "POST",
      body: upstream,
      signal: controller.signal,
    });
    clearTimeout(timeout);
    const text = await resp.text();
    if (!resp.ok) {
      // Surface the orchestrator's structured {error, hint} body to the
      // client so the dropzone can render a useful message instead of a
      // generic "Orchestrator returned 502".
      let inner: { error?: string; hint?: string | null } = {};
      try {
        inner = JSON.parse(text);
      } catch {
        // Non-JSON upstream body; keep raw text in `error` field.
        inner = { error: text || `Orchestrator returned ${resp.status}` };
      }
      return NextResponse.json(
        {
          error: inner.error ?? `Orchestrator returned ${resp.status}`,
          hint: inner.hint ?? null,
          upstream_status: resp.status,
        },
        { status: [400, 413, 415, 422, 429, 503, 504].includes(resp.status) ? resp.status : 502 },
      );
    }
    return new NextResponse(text, {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  } catch (err) {
    clearTimeout(timeout);
    const aborted = err instanceof Error && err.name === "AbortError";
    return NextResponse.json(
      {
        error: aborted
          ? "Preview timed out after 60s."
          : "Sample review service is temporarily unavailable.",
        hint: aborted
          ? "The paper may be very long; try a shorter PDF or retry."
          : "Please try again later.",
      },
      { status: aborted ? 504 : 502 },
    );
  }
}
