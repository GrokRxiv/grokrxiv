import type { User } from "@supabase/supabase-js";
import { createSupabaseServerClient } from "@/lib/supabase/server";

export type UserRole = "user" | "moderator" | "admin" | "super_admin";

export type CurrentUser = {
  user: User | null;
  role: UserRole | null;
};

export function sanitizeNextPath(raw: string | null | undefined): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//")) return "/dashboard";
  return raw;
}

export async function getCurrentUser(): Promise<CurrentUser> {
  const supabase = await createSupabaseServerClient();
  const {
    data: { user },
  } = await supabase.auth.getUser();
  if (!user) return { user: null, role: null };

  const { data } = await supabase
    .from("user_roles")
    .select("role")
    .eq("user_id", user.id)
    .maybeSingle();

  const role = normalizeRole(
    typeof data?.role === "string" ? data.role : "user",
  );
  return { user, role };
}

export function canModerate(role: UserRole | null): boolean {
  return role === "moderator" || canAdmin(role);
}

export function canAdmin(role: UserRole | null): boolean {
  return role === "admin" || role === "super_admin";
}

export function canManageRoles(role: UserRole | null): boolean {
  return role === "super_admin";
}

function normalizeRole(value: string): UserRole {
  if (value === "moderator" || value === "admin" || value === "super_admin") {
    return value;
  }
  return "user";
}
