import Link from "next/link";
import { notFound, redirect } from "next/navigation";
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
import {
  formatSubmissionSourceLabel,
  sourceInfoForPaper,
} from "@/components/source-label";
import { canModerate, getCurrentUser } from "@/lib/auth/server";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import type { Paper } from "@/lib/types";

type ModerationRow = {
  id: string;
  state: string;
  review_id: string;
  notes: string | null;
  created_at: string;
  review:
    | {
        id: string;
        status: string;
        visibility: "public" | "private";
        paper: Paper | null;
      }
    | null;
};

type RawModerationRow = Omit<ModerationRow, "review"> & {
  review: ModerationRow["review"] | ModerationRow["review"][];
};

type SubmissionRow = {
  id: string;
  source: string;
  source_type?: string | null;
  visibility: "public" | "private";
  compute_profile: string;
  state: string;
  created_at: string;
};

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

export default function AdminPage() {
  return (
    <Suspense
      fallback={
        <div className="py-8 text-sm text-[color:var(--color-muted-foreground)]">
          Loading admin console...
        </div>
      }
    >
      <AdminPageContent />
    </Suspense>
  );
}

async function AdminPageContent() {
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/admin");
  if (!canModerate(role)) notFound();

  const supabase = await createSupabaseServerClient();
  const [moderationResult, submissionsResult] = await Promise.all([
    supabase
      .from("moderation_queue")
      .select(
        "id, state, review_id, notes, created_at, review:reviews(id, status, visibility, paper:papers(*))",
      )
      .order("created_at", { ascending: false })
      .limit(25),
    supabase
      .from("submissions")
      .select("id, source, source_type, visibility, compute_profile, state, created_at")
      .order("created_at", { ascending: false })
      .limit(25),
  ]);

  const moderation = normalizeModerationRows(moderationResult.data);
  const submissions = (submissionsResult.data ?? []) as SubmissionRow[];
  const setupError = moderationResult.error || submissionsResult.error;

  return (
    <div className="flex flex-col gap-8 py-8">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Admin console
        </p>
        <h1 className="text-3xl font-bold tracking-tight">Moderation and quota</h1>
        <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
          Public approvals send reviews to the public archive. Private
          approvals release reviews only to the user dashboard and private
          archive.
        </p>
        <div>
          <Button asChild variant="outline">
            <Link href="/admin/users">Manage users</Link>
          </Button>
        </div>
      </header>

      {setupError ? (
        <Card className="border-amber-600 bg-amber-950/20">
          <CardHeader>
            <CardTitle>Admin data unavailable</CardTitle>
            <CardDescription>
              Moderation and submission data could not be loaded. Check the
              application setup before taking moderation action.
            </CardDescription>
          </CardHeader>
        </Card>
      ) : null}

      <section className="flex flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">Moderation queue</h2>
        <div className="grid gap-3">
          {moderation.length === 0 ? (
            <Card>
              <CardContent className="p-6 text-sm text-[color:var(--color-muted-foreground)]">
                No moderation rows.
              </CardContent>
            </Card>
          ) : (
            moderation.map((row) => (
              <Card key={row.id}>
                <CardHeader>
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline">{row.state}</Badge>
                    <Badge variant="secondary">
                      {row.review?.visibility ?? "unknown"}
                    </Badge>
                    <span className="font-mono text-xs text-[color:var(--color-muted-foreground)]">
                      {row.review_id}
                    </span>
                  </div>
                  <CardTitle className="text-base">
                    {row.review?.paper?.title ?? "Review awaiting paper join"}
                  </CardTitle>
                  <CardDescription>
                    {row.review?.paper
                      ? sourceInfoForPaper(row.review.paper).detail
                      : "No paper source"}
                  </CardDescription>
                </CardHeader>
              </Card>
            ))
          )}
        </div>
      </section>

      <section className="flex flex-col gap-4">
        <h2 className="text-2xl font-semibold tracking-tight">Recent submissions</h2>
        <div className="overflow-hidden rounded-lg border border-[color:var(--color-border)]">
          <table className="w-full text-left text-sm">
            <thead className="bg-[color:var(--color-muted)]">
              <tr>
                <th className="px-4 py-3">Source</th>
                <th className="px-4 py-3">Visibility</th>
                <th className="px-4 py-3">Review type</th>
                <th className="px-4 py-3">State</th>
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
                    <Badge variant="outline">{submission.state}</Badge>
                  </td>
                </tr>
              ))}
              {submissions.length === 0 ? (
                <tr>
                  <td
                    colSpan={4}
                    className="px-4 py-6 text-center text-[color:var(--color-muted-foreground)]"
                  >
                    No submissions.
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}

function normalizeModerationRows(data: unknown): ModerationRow[] {
  if (!Array.isArray(data)) return [];
  return data.map((raw) => {
    const row = raw as RawModerationRow;
    return {
      ...row,
      review: Array.isArray(row.review) ? row.review[0] ?? null : row.review,
    };
  });
}
