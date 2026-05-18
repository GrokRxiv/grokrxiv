import { notFound, redirect } from "next/navigation";
import { Suspense } from "react";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { canModerate, getCurrentUser } from "@/lib/auth/server";
import { createSupabaseServerClient } from "@/lib/supabase/server";

type ProfileRow = {
  user_id: string;
  display_name: string | null;
  billing_tier: string;
  review_limit_override: number | null;
  created_at: string;
};

export default function AdminUsersPage() {
  return (
    <Suspense
      fallback={
        <div className="py-8 text-sm text-[color:var(--color-muted-foreground)]">
          Loading users...
        </div>
      }
    >
      <AdminUsersPageContent />
    </Suspense>
  );
}

async function AdminUsersPageContent() {
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/admin/users");
  if (!canModerate(role)) notFound();

  const supabase = await createSupabaseServerClient();
  const { data, error } = await supabase
    .from("profiles")
    .select("user_id, display_name, billing_tier, review_limit_override, created_at")
    .order("created_at", { ascending: false })
    .limit(100);

  const profiles = (data ?? []) as ProfileRow[];

  return (
    <div className="flex flex-col gap-8 py-8">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Admin users
        </p>
        <h1 className="text-3xl font-bold tracking-tight">User quotas</h1>
        <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
          Use this view to audit billing tiers and review limit overrides. A
          write UI for overrides should be added after row-level admin writes
          are fully tested.
        </p>
      </header>

      {error ? (
        <Card className="border-amber-600 bg-amber-950/20">
          <CardHeader>
            <CardTitle>User data unavailable</CardTitle>
            <CardDescription>
              User quota data could not be loaded. Check the application setup
              before changing account limits.
            </CardDescription>
          </CardHeader>
        </Card>
      ) : null}

      <Card>
        <CardContent className="p-0">
          <table className="w-full text-left text-sm">
            <thead className="bg-[color:var(--color-muted)]">
              <tr>
                <th className="px-4 py-3">User</th>
                <th className="px-4 py-3">Tier</th>
                <th className="px-4 py-3">Override</th>
                <th className="px-4 py-3">Created</th>
              </tr>
            </thead>
            <tbody>
              {profiles.map((profile) => (
                <tr
                  key={profile.user_id}
                  className="border-t border-[color:var(--color-border)]"
                >
                  <td className="max-w-xs truncate px-4 py-3">
                    {profile.display_name ?? profile.user_id}
                  </td>
                  <td className="px-4 py-3">
                    <Badge variant="secondary">{profile.billing_tier}</Badge>
                  </td>
                  <td className="px-4 py-3">
                    {profile.review_limit_override ?? "default"}
                  </td>
                  <td className="px-4 py-3">
                    {new Date(profile.created_at).toLocaleDateString()}
                  </td>
                </tr>
              ))}
              {profiles.length === 0 ? (
                <tr>
                  <td
                    colSpan={4}
                    className="px-4 py-6 text-center text-[color:var(--color-muted-foreground)]"
                  >
                    No profiles.
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </CardContent>
      </Card>
    </div>
  );
}
