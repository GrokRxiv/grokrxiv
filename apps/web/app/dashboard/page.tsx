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
import { getCurrentUser } from "@/lib/auth/server";
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

function freeReviewLimit(): number {
  const parsed = Number.parseInt(process.env.GROKRXIV_FREE_REVIEW_LIMIT ?? "3", 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 3;
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

export default function DashboardPage() {
  return (
    <Suspense
      fallback={
        <div className="py-8 text-sm text-[color:var(--color-muted-foreground)]">
          Loading dashboard...
        </div>
      }
    >
      <DashboardPageContent />
    </Suspense>
  );
}

async function DashboardPageContent() {
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/dashboard");

  const supabase = await createSupabaseServerClient();
  const [profileResult, submissionsResult, usedResult] = await Promise.all([
    supabase
      .from("profiles")
      .select("billing_tier, review_limit_override")
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
      .select("id", { count: "exact", head: true })
      .eq("user_id", user.id)
      .neq("compute_profile", "sample_preview")
      .in("state", COUNTED_FULL_REVIEW_STATES as unknown as string[]),
  ]);

  const profile = profileResult.data as ProfileRow | null;
  const setupError =
    profileResult.error || submissionsResult.error || usedResult.error;
  const used = usedResult.count ?? 0;
  const limit = profile?.review_limit_override ?? freeReviewLimit();
  const remaining = Math.max(0, limit - used);
  const submissions = (submissionsResult.data ?? []) as SubmissionRow[];

  return (
    <div className="flex flex-col gap-8 py-8">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Account dashboard
        </p>
        <h1 className="text-3xl font-bold tracking-tight">Your GrokRxiv reviews</h1>
        <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
          Full paper reviews will appear here as they move through review and
          moderation. The homepage PDF upload remains a sample preview and does
          not consume full-review quota.
        </p>
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

      <section className="grid gap-4 md:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle>Plan</CardTitle>
            <CardDescription>
              {profile?.billing_tier ?? "free"} account
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Badge variant="secondary">{role ?? "user"}</Badge>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Full reviews</CardTitle>
            <CardDescription>
              {used} / {limit} used
            </CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{remaining}</p>
            <p className="text-sm text-[color:var(--color-muted-foreground)]">
              remaining on the free cap
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Visibility</CardTitle>
            <CardDescription>
              Free full reviews are public. Private reviews require a paid
              plan.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="flex flex-wrap gap-2">
              <Button asChild variant="outline">
                <Link href="/#upload">Run sample preview</Link>
              </Button>
              <Button asChild variant="ghost">
                <Link href="/pricing">View pricing</Link>
              </Button>
            </div>
          </CardContent>
        </Card>
      </section>

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
                      {submission.source}
                    </td>
                    <td className="px-4 py-3">{submission.visibility}</td>
                    <td className="px-4 py-3">
                      {formatReviewType(submission.compute_profile)}
                    </td>
                    <td className="px-4 py-3">
                      <Badge variant="outline">{submission.state}</Badge>
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
