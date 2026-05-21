import { NextResponse } from "next/server";
import { canAdmin, canManageRoles, getCurrentUser } from "@/lib/auth/server";

export async function requireAdminUser(request: Request): Promise<
  | { ok: true; userId: string; canManageRoles: boolean }
  | { ok: false; response: NextResponse }
> {
  const { user, role } = await getCurrentUser();
  if (!user) {
    const url = new URL(request.url);
    return {
      ok: false,
      response: NextResponse.redirect(new URL("/login?next=/admin/users", url), {
        status: 303,
      }),
    };
  }
  if (!canAdmin(role)) {
    return {
      ok: false,
      response: NextResponse.json({ error: "forbidden" }, { status: 403 }),
    };
  }
  return { ok: true, userId: user.id, canManageRoles: canManageRoles(role) };
}

export function adminUsersRedirect(request: Request, status: string): NextResponse {
  const url = new URL(request.url);
  const next = new URL("/admin/users", url);
  next.searchParams.set("status", status);
  return NextResponse.redirect(next, { status: 303 });
}
