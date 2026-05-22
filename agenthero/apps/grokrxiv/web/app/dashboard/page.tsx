import Link from "next/link";
import { redirect } from "next/navigation";
import { Suspense } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { formatSubmissionSourceLabel } from "@/components/source-label";
import { getCurrentUser } from "@/lib/auth/server";
import { billingConfigured } from "@/lib/billing";
import { createSupabaseServerClient } from "@/lib/supabase/server";

const COUNTED_FULL_REVIEW_STATES = [
  "queued",
  "running",
  "awaiting_moderation",
  "pr_open",
  "published",
  "corrected",
  "rejected",
  "private_ready",
  "cancelled",
] as const;

type SearchParams = {
  submit?: string;
};

type SubmissionRow = {
  id: string;
  source: string;
  source_type: string;
  visibility: "public" | "private";
  compute_profile: string;
  state: string;
  review_id: string | null;
  quota_charged: boolean;
  created_at: string;
};

type ProfileRow = {
  billing_tier: string;
  review_limit_override: number | null;
};

type BillingRow = {
  plan_id: string;
  status: string;
  period_start: string | null;
  period_end: string | null;
  stripe_customer_id: string | null;
};

type PlanRow = {
  id: string;
  name: string;
  public_reviews_per_month: number;
  private_reviews_per_month: number;
  lifetime_public_reviews: number | null;
  allow_private: boolean;
};

type QuotaSummary = {
  planId: string;
  planName: string;
  publicUsed: number;
  publicLimit: number;
  privateUsed: number;
  privateLimit: number;
  publicRemaining: number;
  privateRemaining: number;
  allowPrivate: boolean;
  paidActive: boolean;
};

function freeReviewLimit(): number {
  const parsed = Number.parseInt(process.env.GROKRXIV_FREE_REVIEW_LIMIT ?? "3", 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 3;
}

function activePlanId(profile: ProfileRow | null, billing: BillingRow | null): string {
  const billingActive = billing?.status === "active" || billing?.status === "trialing";
  if (billingActive && billing?.plan_id) return billing.plan_id;
  return profile?.billing_tier === "admin" ? "admin" : "free";
}

function currentMonthWindow(billing: BillingRow | null): { start: Date; end: Date } {
  const now = new Date();
  const start = billing?.period_start ? new Date(billing.period_start) : new Date(now);
  if (!billing?.period_start || Number.isNaN(start.getTime())) {
    start.setUTCDate(1);
    start.setUTCHours(0, 0, 0, 0);
  }
  const end = billing?.period_end ? new Date(billing.period_end) : new Date(start);
  if (!billing?.period_end || Number.isNaN(end.getTime())) {
    end.setUTCMonth(end.getUTCMonth() + 1);
  }
  return { start, end };
}

function isCountedSubmissionState(state: string): boolean {
  return (COUNTED_FULL_REVIEW_STATES as readonly string[]).includes(state);
}

function countedInWindow(
  submission: SubmissionRow,
  start: Date,
  end: Date,
): boolean {
  if (!submission.quota_charged) return false;
  if (!isCountedSubmissionState(submission.state)) return false;
  const createdAt = new Date(submission.created_at);
  return createdAt >= start && createdAt < end;
}

function buildQuotaSummary(input: {
  profile: ProfileRow | null;
  billing: BillingRow | null;
  plan: PlanRow | null;
  submissions: SubmissionRow[];
}): QuotaSummary {
  const planId = activePlanId(input.profile, input.billing);
  const plan = input.plan;
  const { start, end } = currentMonthWindow(input.billing);
  const paidActive = planId !== "free";
  const publicUsed = input.submissions.filter((submission) => {
    if (submission.visibility !== "public") return false;
    if (submission.compute_profile === "sample_preview") return false;
    return paidActive
      ? countedInWindow(submission, start, end)
      : submission.quota_charged &&
          isCountedSubmissionState(submission.state);
  }).length;
  const privateUsed = input.submissions.filter(
    (submission) =>
      submission.visibility === "private" &&
      submission.compute_profile !== "sample_preview" &&
      countedInWindow(submission, start, end),
  ).length;
  const publicLimit =
    input.profile?.review_limit_override ??
    (paidActive
      ? plan?.public_reviews_per_month ?? 0
      : plan?.lifetime_public_reviews ?? freeReviewLimit());
  const privateLimit = paidActive ? plan?.private_reviews_per_month ?? 0 : 0;
  return {
    planId,
    planName: plan?.name ?? planId,
    publicUsed,
    publicLimit,
    privateUsed,
    privateLimit,
    publicRemaining: Math.max(0, publicLimit - publicUsed),
    privateRemaining: Math.max(0, privateLimit - privateUsed),
    allowPrivate: paidActive && (plan?.allow_private ?? false),
    paidActive,
  };
}

function submitStatusMessage(status: string | undefined): string | null {
  switch (status) {
    case "queued":
      return "Review queued. It will appear in submissions while the supervisor runs.";
    case "bad_source":
      return "Enter a valid arXiv identifier.";
    case "public_consent_required":
      return "Public reviews require publication consent before queuing.";
    case "private_reviews_require_paid_plan":
      return "Private reviews require an active paid plan.";
    case "public_quota_exhausted":
      return "Public review quota is exhausted for this account.";
    case "private_quota_exhausted":
      return "Private review quota is exhausted for this billing period.";
    case "orchestrator_unreachable":
    case "orchestrator_unavailable":
    case "dispatch_failed":
      return "The review supervisor did not accept the job. Quota was refunded.";
    case "claim_failed":
      return "The review could not be queued. Check account setup and try again.";
    default:
      return null;
  }
}

function formatReviewType(value: string): string {
  switch (value) {
    case "sample_preview":
      return "Sample preview";
    case "public_free":
      return "Public full review";
    case "paid_standard":
      return "Public paid review";
    case "paid_private":
      return "Private review";
    case "premium_api":
      return "Premium review";
    default:
      return value.replaceAll("_", " ");
  }
}

function formatSubmissionSource(submission: SubmissionRow): string {
  return formatSubmissionSourceLabel(submission.source, submission.source_type);
}

export default function DashboardPage({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  return (
    <Suspense
      fallback={
        <div className="py-8 text-sm text-[color:var(--color-muted-foreground)]">
          Loading dashboard...
        </div>
      }
    >
      <DashboardPageContent searchParams={searchParams} />
    </Suspense>
  );
}

async function DashboardPageContent({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  const { submit } = await searchParams;
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/dashboard");

  const supabase = await createSupabaseServerClient();
  const billingEnabled = billingConfigured();
  const [profileResult, billingResult, submissionsResult, quotaRowsResult, plansResult] =
    await Promise.all([
    supabase
      .from("profiles")
      .select("billing_tier, review_limit_override")
      .eq("user_id", user.id)
      .maybeSingle(),
    supabase
      .from("user_billing")
      .select("plan_id, status, period_start, period_end, stripe_customer_id")
      .eq("user_id", user.id)
      .maybeSingle(),
    supabase
      .from("submissions")
      .select(
        "id, source, source_type, visibility, compute_profile, state, review_id, quota_charged, created_at",
      )
      .eq("user_id", user.id)
      .order("created_at", { ascending: false })
      .limit(25),
    supabase
      .from("submissions")
      .select(
        "id, source, source_type, visibility, compute_profile, state, review_id, quota_charged, created_at",
      )
      .eq("user_id", user.id)
      .neq("compute_profile", "sample_preview")
      .in("state", COUNTED_FULL_REVIEW_STATES as unknown as string[]),
    supabase
      .from("billing_plans")
      .select(
        "id, name, public_reviews_per_month, private_reviews_per_month, lifetime_public_reviews, allow_private",
      ),
  ]);

  const profile = profileResult.data as ProfileRow | null;
  const billing = billingResult.data as BillingRow | null;
  const setupError =
    profileResult.error ||
    billingResult.error ||
    submissionsResult.error ||
    quotaRowsResult.error ||
    plansResult.error;
  const submissions = (submissionsResult.data ?? []) as SubmissionRow[];
  const quotaRows = (quotaRowsResult.data ?? []) as SubmissionRow[];
  const plans = (plansResult.data ?? []) as PlanRow[];
  const planId = activePlanId(profile, billing);
  const plan = plans.find((candidate) => candidate.id === planId) ?? null;
  const quota = buildQuotaSummary({ profile, billing, plan, submissions: quotaRows });
  const submitMessage = submitStatusMessage(submit);
  const canQueueReview =
    quota.publicRemaining > 0 || (quota.allowPrivate && quota.privateRemaining > 0);

  return (
    <div className="flex flex-col gap-8 py-8">
      <header className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex flex-col gap-3">
          <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
            Account dashboard
          </p>
          <h1 className="text-3xl font-bold tracking-tight">Your GrokRxiv reviews</h1>
          <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
            Full paper reviews will appear here as they move through review and
            moderation. The homepage PDF upload remains a sample preview and does
            not consume full-review quota.
          </p>
        </div>
        <form action="/auth/sign-out" method="post">
          <Button type="submit" variant="outline">
            Sign out
          </Button>
        </form>
      </header>

      {setupError ? (
        <Card className="border-amber-600 bg-amber-950/20">
          <CardHeader>
            <CardTitle>Account data unavailable</CardTitle>
            <CardDescription>
              Dashboard limits and submissions are temporarily unavailable.
              Public reviews remain available.
            </CardDescription>
          </CardHeader>
        </Card>
      ) : null}

      {submitMessage ? (
        <Card
          className={
            submit === "queued"
              ? "border-emerald-600 bg-emerald-950/20"
              : "border-amber-600 bg-amber-950/20"
          }
        >
          <CardContent className="p-4 text-sm">{submitMessage}</CardContent>
        </Card>
      ) : null}

      <section className="grid gap-4 md:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle>Plan</CardTitle>
            <CardDescription>
              {quota.planName} account
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            <div className="flex flex-wrap gap-2">
              <Badge variant="secondary">{role ?? "user"}</Badge>
              <Badge variant="outline">{billing?.status ?? "active"}</Badge>
            </div>
            {billing?.period_end ? (
              <p className="text-sm text-[color:var(--color-muted-foreground)]">
                Renews {new Date(billing.period_end).toLocaleDateString()}
              </p>
            ) : null}
            {billingEnabled && billing?.stripe_customer_id ? (
              <form action="/api/billing/portal" method="post">
                <Button type="submit" variant="outline" size="sm">
                  Manage billing
                </Button>
              </form>
            ) : billingEnabled ? (
              <Button asChild variant="outline" size="sm">
                <Link href="/pricing">Upgrade plan</Link>
              </Button>
            ) : null}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Public reviews</CardTitle>
            <CardDescription>
              {quota.publicUsed} / {quota.publicLimit} used
            </CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{quota.publicRemaining}</p>
            <p className="text-sm text-[color:var(--color-muted-foreground)]">
              {quota.paidActive ? "remaining this period" : "free lifetime reviews remaining"}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Private reviews</CardTitle>
            <CardDescription>
              {quota.privateUsed} / {quota.privateLimit} used this period
            </CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{quota.privateRemaining}</p>
            <p className="text-sm text-[color:var(--color-muted-foreground)]">
              {quota.allowPrivate ? "private slots available" : "upgrade required"}
            </p>
          </CardContent>
        </Card>
      </section>

      <Card>
        <CardHeader>
          <CardTitle>Run a full review</CardTitle>
          <CardDescription>
            Queue an arXiv paper for the same multi-agent review used by the
            CLI. Public reviews can be published after moderation.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form action="/api/account/reviews" method="post" className="grid gap-5">
            <div className="grid gap-2">
              <label className="text-sm font-medium" htmlFor="review-source">
                arXiv ID
              </label>
              <input
                id="review-source"
                name="source"
                type="text"
                inputMode="text"
                placeholder="2605.17307"
                className="h-11 rounded-md border border-[color:var(--color-border)] bg-transparent px-3 font-mono text-sm outline-none ring-offset-background placeholder:text-[color:var(--color-muted-foreground)] focus-visible:ring-2 focus-visible:ring-[color:var(--color-ring)]"
                required
              />
            </div>
            <fieldset className="grid gap-3">
              <legend className="text-sm font-medium">Visibility</legend>
              <div className="grid gap-3 sm:grid-cols-2">
                <label className="flex min-h-24 cursor-pointer flex-col gap-2 rounded-md border border-[color:var(--color-border)] p-4">
                  <span className="flex items-center gap-2 text-sm font-semibold">
                    <input
                      type="radio"
                      name="visibility"
                      value="public"
                      defaultChecked={quota.publicRemaining > 0 || !quota.allowPrivate}
                      className="size-4"
                    />
                    Public
                  </span>
                  <span className="text-sm text-[color:var(--color-muted-foreground)]">
                    Uses public quota. The review can appear on GrokRxiv after
                    moderation.
                  </span>
                </label>
                <label
                  className={
                    quota.allowPrivate
                      ? "flex min-h-24 cursor-pointer flex-col gap-2 rounded-md border border-[color:var(--color-border)] p-4"
                      : "flex min-h-24 cursor-not-allowed flex-col gap-2 rounded-md border border-[color:var(--color-border)] p-4 opacity-60"
                  }
                >
                  <span className="flex items-center gap-2 text-sm font-semibold">
                    <input
                      type="radio"
                      name="visibility"
                      value="private"
                      disabled={!quota.allowPrivate}
                      defaultChecked={
                        quota.publicRemaining <= 0 &&
                        quota.allowPrivate &&
                        quota.privateRemaining > 0
                      }
                      className="size-4"
                    />
                    Private
                  </span>
                  <span className="text-sm text-[color:var(--color-muted-foreground)]">
                    {quota.allowPrivate
                      ? "Visible to your account and moderators."
                      : "Available on paid plans."}
                  </span>
                </label>
              </div>
            </fieldset>
            <label className="flex items-start gap-3 text-sm">
              <input
                type="checkbox"
                name="public_consent"
                value="true"
                className="mt-1 size-4"
                required
              />
              <span>
                I understand public full reviews may be published after
                moderation, including major-revision or rejection outcomes.
              </span>
            </label>
            <div className="flex flex-wrap items-center gap-3">
              <Button type="submit" disabled={!canQueueReview}>
                Queue review
              </Button>
              <Button asChild variant="outline">
                <Link href="/#sample-review">Run sample preview</Link>
              </Button>
              {!quota.allowPrivate ? (
                <Button asChild variant="ghost">
                  <Link href="/pricing">View pricing</Link>
                </Button>
              ) : null}
            </div>
          </form>
        </CardContent>
      </Card>

      <section className="flex flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">Submissions</h2>
        {submissions.length === 0 ? (
          <Card>
            <CardContent className="p-6 text-sm text-[color:var(--color-muted-foreground)]">
              No full-review submissions yet.
            </CardContent>
          </Card>
        ) : (
          <div className="overflow-hidden rounded-lg border border-[color:var(--color-border)]">
            <table className="w-full text-left text-sm">
              <thead className="bg-[color:var(--color-muted)]">
                <tr>
                  <th className="px-4 py-3">Source</th>
                  <th className="px-4 py-3">Visibility</th>
                  <th className="px-4 py-3">Review type</th>
                  <th className="px-4 py-3">State</th>
                  <th className="px-4 py-3">Created</th>
                </tr>
              </thead>
              <tbody>
                {submissions.map((submission) => (
                  <tr
                    key={submission.id}
                    className="border-t border-[color:var(--color-border)]"
                  >
                    <td className="max-w-xs truncate px-4 py-3 font-mono">
                      {formatSubmissionSource(submission)}
                    </td>
                    <td className="px-4 py-3">{submission.visibility}</td>
                    <td className="px-4 py-3">
                      {formatReviewType(submission.compute_profile)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex flex-wrap items-center gap-2">
                        <Badge variant="outline">{submission.state}</Badge>
                        {submission.review_id ? (
                          <Link
                            href={`/reviews/${submission.review_id}`}
                            className="text-xs font-medium text-[color:var(--color-primary)] hover:underline"
                          >
                            Open review
                          </Link>
                        ) : null}
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      {new Date(submission.created_at).toLocaleDateString()}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
