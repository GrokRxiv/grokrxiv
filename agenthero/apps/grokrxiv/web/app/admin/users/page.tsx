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
import { Button } from "@/components/ui/button";
import { canAdmin, canManageRoles, getCurrentUser } from "@/lib/auth/server";
import { createSupabaseServerClient } from "@/lib/supabase/server";

type SearchParams = {
  status?: string;
};

type ProfileRow = {
  user_id: string;
  display_name: string | null;
  billing_tier: string;
  review_limit_override: number | null;
  created_at: string;
};

type RoleRow = {
  user_id: string;
  role: string;
};

type BillingRow = {
  user_id: string;
  plan_id: string;
  status: string;
};

export default function AdminUsersPage({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  return (
    <Suspense
      fallback={
        <div className="py-8 text-sm text-[color:var(--color-muted-foreground)]">
          Loading users...
        </div>
      }
    >
      <AdminUsersPageContent searchParams={searchParams} />
    </Suspense>
  );
}

async function AdminUsersPageContent({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  const { status } = await searchParams;
  const { user, role } = await getCurrentUser();
  if (!user) redirect("/login?next=/admin/users");
  if (!canAdmin(role)) notFound();

  const supabase = await createSupabaseServerClient();
  const [profilesResult, rolesResult, billingResult] = await Promise.all([
    supabase
      .from("profiles")
      .select("user_id, display_name, billing_tier, review_limit_override, created_at")
      .order("created_at", { ascending: false })
      .limit(100),
    supabase.from("user_roles").select("user_id, role"),
    supabase.from("user_billing").select("user_id, plan_id, status"),
  ]);

  const profiles = (profilesResult.data ?? []) as ProfileRow[];
  const roles = new Map(
    ((rolesResult.data ?? []) as RoleRow[]).map((row) => [row.user_id, row.role]),
  );
  const billing = new Map(
    ((billingResult.data ?? []) as BillingRow[]).map((row) => [row.user_id, row]),
  );
  const error = profilesResult.error || rolesResult.error || billingResult.error;
  const roleManagementAllowed = canManageRoles(role);

  return (
    <div className="flex flex-col gap-8 py-8">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Admin users
        </p>
        <h1 className="text-3xl font-bold tracking-tight">User quotas</h1>
        <p className="max-w-3xl text-[color:var(--color-muted-foreground)]">
          Audit roles, billing plans, and review limit overrides. Billing and
          quota updates use service-role writes after the current admin is
          checked server-side.
        </p>
      </header>

      {status ? (
        <Card className="border-emerald-600 bg-emerald-950/20">
          <CardContent className="p-4 text-sm">
            Account control update: {status.replaceAll("_", " ")}.
          </CardContent>
        </Card>
      ) : null}

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
                <th className="px-4 py-3">Role</th>
                <th className="px-4 py-3">Tier</th>
                <th className="px-4 py-3">Override</th>
                <th className="px-4 py-3">Created</th>
                <th className="px-4 py-3">Controls</th>
              </tr>
            </thead>
            <tbody>
              {profiles.map((profile) => {
                const billingRow = billing.get(profile.user_id);
                const userRole = roles.get(profile.user_id) ?? "user";
                return (
                  <tr
                    key={profile.user_id}
                    className="border-t border-[color:var(--color-border)] align-top"
                  >
                    <td className="max-w-xs truncate px-4 py-3">
                      {profile.display_name ?? profile.user_id}
                    </td>
                    <td className="px-4 py-3">
                      <Badge variant="outline">{userRole}</Badge>
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex flex-col gap-1">
                        <Badge variant="secondary">
                          {billingRow?.plan_id ?? profile.billing_tier}
                        </Badge>
                        <span className="text-xs text-[color:var(--color-muted-foreground)]">
                          {billingRow?.status ?? "active"}
                        </span>
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      {profile.review_limit_override ?? "default"}
                    </td>
                    <td className="px-4 py-3">
                      {new Date(profile.created_at).toLocaleDateString()}
                    </td>
                    <td className="min-w-80 px-4 py-3">
                      <div className="grid gap-3">
                        {roleManagementAllowed ? (
                          <form
                            action={`/api/admin/users/${profile.user_id}/role`}
                            method="post"
                            className="flex flex-wrap gap-2"
                          >
                            <select
                              name="role"
                              defaultValue={userRole}
                              className="h-9 rounded-md border border-[color:var(--color-border)] bg-transparent px-2 text-sm"
                            >
                              <option value="user">user</option>
                              <option value="moderator">moderator</option>
                              <option value="admin">admin</option>
                              <option value="super_admin">super admin</option>
                            </select>
                            <Button type="submit" size="sm" variant="outline">
                              Update role
                            </Button>
                          </form>
                        ) : null}
                        <form
                          action={`/api/admin/users/${profile.user_id}/billing`}
                          method="post"
                          className="flex flex-wrap gap-2"
                        >
                          <select
                            name="plan_id"
                            defaultValue={billingRow?.plan_id ?? profile.billing_tier}
                            className="h-9 rounded-md border border-[color:var(--color-border)] bg-transparent px-2 text-sm"
                          >
                            <option value="free">free</option>
                            <option value="supporter">supporter</option>
                            <option value="researcher">researcher</option>
                            <option value="admin">admin</option>
                          </select>
                          <select
                            name="status"
                            defaultValue={billingRow?.status ?? "active"}
                            className="h-9 rounded-md border border-[color:var(--color-border)] bg-transparent px-2 text-sm"
                          >
                            <option value="active">active</option>
                            <option value="trialing">trialing</option>
                            <option value="past_due">past due</option>
                            <option value="canceled">canceled</option>
                          </select>
                          <Button type="submit" size="sm" variant="outline">
                            Update plan
                          </Button>
                        </form>
                        <form
                          action={`/api/admin/users/${profile.user_id}/quota`}
                          method="post"
                          className="flex flex-wrap gap-2"
                        >
                          <input
                            name="review_limit_override"
                            type="number"
                            min="0"
                            placeholder="default"
                            defaultValue={profile.review_limit_override ?? ""}
                            className="h-9 w-28 rounded-md border border-[color:var(--color-border)] bg-transparent px-2 text-sm"
                          />
                          <Button type="submit" size="sm" variant="outline">
                            Update quota
                          </Button>
                        </form>
                      </div>
                    </td>
                  </tr>
                );
              })}
              {profiles.length === 0 ? (
                <tr>
                  <td
                    colSpan={6}
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
