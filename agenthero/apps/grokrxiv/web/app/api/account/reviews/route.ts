import { NextResponse } from "next/server";
import { z } from "zod";
import { getCurrentUser } from "@/lib/auth/server";
import { ORCHESTRATOR_INTERNAL_URL } from "@/lib/env";
import { createSupabaseServiceClient } from "@/lib/supabase/service";

const Body = z.object({
  source: z.string().min(1).max(128),
  visibility: z.enum(["public", "private"]).default("public"),
  public_consent: z.coerce.boolean().default(false),
});

const ARXIV_ID = /^(?:arxiv:)?([a-z-]+\/\d{7}|\d{4}\.\d{4,5})(v\d+)?$/i;

type ClaimRow = {
  submission_id: string;
  plan_id: string;
  compute_profile: string;
  visibility: "public" | "private";
};

export async function POST(request: Request) {
  const url = new URL(request.url);
  const { user } = await getCurrentUser();
  if (!user) {
    return NextResponse.redirect(new URL("/login?next=/dashboard", url), {
      status: 303,
    });
  }

  const form = await request.formData().catch(() => null);
  const parsed = Body.safeParse(form ? Object.fromEntries(form) : {});
  if (!parsed.success) {
    return redirectWithStatus(url, "bad_source");
  }

  const source = normalizeArxivId(parsed.data.source);
  if (!source) {
    return redirectWithStatus(url, "bad_source");
  }
  if (parsed.data.visibility === "public" && !parsed.data.public_consent) {
    return redirectWithStatus(url, "public_consent_required");
  }

  const supabase = createSupabaseServiceClient();
  await supabase.rpc("grokrxiv_ensure_user_account", {
    target_user_id: user.id,
    target_email: user.email ?? null,
  });

  const { data, error } = await supabase.rpc("grokrxiv_claim_review_submission", {
    target_user_id: user.id,
    target_source: source,
    target_source_type: "arxiv",
    target_visibility: parsed.data.visibility,
    requested_compute_profile: null,
    target_public_consent: parsed.data.public_consent,
  });
  if (error) {
    return redirectWithStatus(url, quotaErrorCode(error.message));
  }

  const claim = Array.isArray(data) ? (data[0] as ClaimRow | undefined) : null;
  if (!claim?.submission_id) {
    return redirectWithStatus(url, "claim_failed");
  }

  const response = await dispatchReview({
    source,
    visibility: claim.visibility,
    computeProfile: claim.compute_profile,
    userId: user.id,
    submissionId: claim.submission_id,
    publicConsent: parsed.data.public_consent,
  });
  if (!response.ok) {
    await supabase.rpc("grokrxiv_mark_submission_failed", {
      target_submission_id: claim.submission_id,
      target_error: response.error,
      refund_quota: true,
    });
    return redirectWithStatus(url, response.code);
  }

  return redirectWithStatus(url, "queued");
}

function normalizeArxivId(raw: string): string | null {
  const trimmed = raw.trim();
  const match = trimmed.match(ARXIV_ID);
  if (!match) return null;
  return `${match[1]}${match[2] ?? ""}`;
}

function redirectWithStatus(url: URL, status: string): NextResponse {
  const next = new URL("/dashboard", url);
  next.searchParams.set("submit", status);
  return NextResponse.redirect(next, { status: 303 });
}

function quotaErrorCode(message: string): string {
  const known = [
    "public_consent_required",
    "private_reviews_require_paid_plan",
    "public_quota_exhausted",
    "private_quota_exhausted",
    "bad_visibility",
    "bad_compute_profile",
  ];
  return known.find((code) => message.includes(code)) ?? "claim_failed";
}

async function dispatchReview(input: {
  source: string;
  visibility: "public" | "private";
  computeProfile: string;
  userId: string;
  submissionId: string;
  publicConsent: boolean;
}): Promise<{ ok: true } | { ok: false; code: string; error: string }> {
  let response: Response;
  try {
    response = await fetch(`${ORCHESTRATOR_INTERNAL_URL}/internal/v1/review`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        source: input.source,
        type: "arxiv",
        visibility: input.visibility,
        compute_profile: input.computeProfile,
        public_consent: input.publicConsent,
        submitted_by: input.userId,
        submission_id: input.submissionId,
      }),
    });
  } catch (error) {
    return {
      ok: false,
      code: "orchestrator_unreachable",
      error: String(error),
    };
  }
  if (response.ok) return { ok: true };
  const body = await response.text().catch(() => "");
  return {
    ok: false,
    code: response.status === 503 ? "orchestrator_unavailable" : "dispatch_failed",
    error: body || `orchestrator returned ${response.status}`,
  };
}
