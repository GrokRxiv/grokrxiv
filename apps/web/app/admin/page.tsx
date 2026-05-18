import Link from "next/link";
import { notFound, redirect } from "next/navigation";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { canModerate, getCurrentUser } from "@/lib/auth/server";
import { createSupabaseServerClient } from "@/lib/supabase/server";

export const dynamic = "force-dynamic";

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
        paper:
          | {
              arxiv_id: string;
              title: string;
            }
          | null;
      }
    | null;
};

type RawModerationRow = Omit<ModerationRow, "review"> & {
  review: ModerationRow["review"] | ModerationRow["review"][];
};

type SubmissionRow = {
  id: string;
  source: string;
  visibility: "public" | "private";
  compute_profile: string;
  state: string;
  created_at: string;
};

export default async function AdminPage() {
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/admin");
  if (!canModerate(role)) notFound();

  const supabase = await createSupabaseServerClient();
  const [moderationResult, submissionsResult] = await Promise.all([
    supabase
      .from("moderation_queue")
      .select(
        "id, state, review_id, notes, created_at, review:reviews(id, status, visibility, paper:papers(arxiv_id, title))",
      )
      .order("created_at", { ascending: false })
      .limit(25),
    supabase
      .from("submissions")
      .select("id, source, visibility, compute_profile, state, created_at")
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
          Public approvals open PRs to the public review repo. Private approvals
          release the review to the user dashboard and optional private archive.
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
            <CardTitle>Admin schema not applied</CardTitle>
            <CardDescription>
              Apply the latest Supabase migration before using auth, quotas, or
              private-review moderation.
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
                    {row.review?.paper?.arxiv_id
                      ? `arXiv:${row.review.paper.arxiv_id}`
                      : "No arXiv id"}
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
                <th className="px-4 py-3">Profile</th>
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
                    {submission.source}
                  </td>
                  <td className="px-4 py-3">{submission.visibility}</td>
                  <td className="px-4 py-3">{submission.compute_profile}</td>
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
