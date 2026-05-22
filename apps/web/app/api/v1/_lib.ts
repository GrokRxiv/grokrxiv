// Shared helpers for legacy operator write endpoints.
//
// These endpoints require a bearer token (`AGENTHERO_SERVICE_TOKEN`) and forward
// requests to the orchestrator's internal HTTP API. Read endpoints
// (e.g. /api/v1/reviews) talk to Supabase directly and don't go through here.

import { NextResponse } from "next/server";
import { ORCHESTRATOR_INTERNAL_URL } from "@/lib/env";

export function requireServiceToken(req: Request): NextResponse | null {
  const token = process.env.AGENTHERO_SERVICE_TOKEN ?? "";
  if (!token) {
    return NextResponse.json(
      { error: "service_unconfigured", detail: "AGENTHERO_SERVICE_TOKEN is unset" },
      { status: 503 },
    );
  }
  const auth = req.headers.get("authorization") ?? "";
  if (auth !== `Bearer ${token}`) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }
  return null;
}

/**
 * Forward a JSON body to a path on the orchestrator's internal HTTP API.
 * Returns the orchestrator's response (status + body) verbatim. If the
 * orchestrator is unreachable we surface 502 so the caller can distinguish
 * "auth ok but backend down" from "auth bad".
 */
export async function forwardToOrchestrator(
  path: string,
  init: RequestInit & { body?: BodyInit | null },
): Promise<Response> {
  const url = `${ORCHESTRATOR_INTERNAL_URL}${path}`;
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      headers: {
        "content-type": "application/json",
        ...(init.headers ?? {}),
      },
    });
  } catch (err) {
    return NextResponse.json(
      { error: "orchestrator_unreachable", detail: String(err) },
      { status: 502 },
    );
  }
  const ct = res.headers.get("content-type") ?? "";
  const body = ct.includes("application/json")
    ? await res.json().catch(() => ({}))
    : await res.text().catch(() => "");
  return NextResponse.json(body, { status: res.status });
}

/**
 * Convenience for "endpoint not yet wired" stubs. Returns 501 with a clear
 * pointer at the follow-up track.
 */
export function notImplemented(detail: string): NextResponse {
  return NextResponse.json({ error: "not_implemented", detail }, { status: 501 });
}
