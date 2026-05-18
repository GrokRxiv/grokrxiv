import type { User } from "@supabase/supabase-js";
import { createSupabaseServerClient } from "@/lib/supabase/server";

export type UserRole = "user" | "moderator" | "admin";

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
  return role === "moderator" || role === "admin";
}

function normalizeRole(value: string): UserRole {
  if (value === "moderator" || value === "admin") return value;
  return "user";
}
