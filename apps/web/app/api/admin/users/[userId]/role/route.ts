import { NextResponse } from "next/server";
import { z } from "zod";
import { createSupabaseServiceClient } from "@/lib/supabase/service";
import { adminUsersRedirect, requireAdminUser } from "../../_lib";

const Body = z.object({
  role: z.enum(["user", "moderator", "admin", "super_admin"]),
});

export async function POST(
  request: Request,
  { params }: { params: Promise<{ userId: string }> },
) {
  const access = await requireAdminUser(request);
  if (!access.ok) return access.response;
  if (!access.canManageRoles) {
    return NextResponse.json({ error: "super_admin_required" }, { status: 403 });
  }

  const { userId } = await params;
  const form = await request.formData().catch(() => null);
  const parsed = Body.safeParse(form ? Object.fromEntries(form) : {});
  if (!parsed.success) {
    return adminUsersRedirect(request, "bad_role");
  }

  const supabase = createSupabaseServiceClient();
  if (parsed.data.role !== "super_admin") {
    const { count } = await supabase
      .from("user_roles")
      .select("user_id", { count: "exact", head: true })
      .eq("role", "super_admin");
    const { data: current } = await supabase
      .from("user_roles")
      .select("role")
      .eq("user_id", userId)
      .maybeSingle();
    if (current?.role === "super_admin" && (count ?? 0) <= 1) {
      return adminUsersRedirect(request, "last_super_admin");
    }
  }

  const { error } = await supabase.from("user_roles").upsert(
    {
      user_id: userId,
      role: parsed.data.role,
      updated_at: new Date().toISOString(),
    },
    { onConflict: "user_id" },
  );
  if (error) {
    return adminUsersRedirect(request, "role_update_failed");
  }
  return adminUsersRedirect(request, "role_updated");
}
