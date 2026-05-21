import { NextResponse } from "next/server";
import { createSupabaseServerClient } from "@/lib/supabase/server";
import { sanitizeNextPath } from "@/lib/auth/server";

export async function GET(request: Request) {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const authError =
    url.searchParams.get("error_description") ?? url.searchParams.get("error");
  const next = sanitizeNextPath(url.searchParams.get("next"));
  if (authError) {
    const login = new URL("/login", url.origin);
    login.searchParams.set("next", next);
    login.searchParams.set("error", authError);
    return NextResponse.redirect(login);
  }
  if (code) {
    const supabase = await createSupabaseServerClient();
    const { error } = await supabase.auth.exchangeCodeForSession(code);
    if (error) {
      const login = new URL("/login", url.origin);
      login.searchParams.set("next", next);
      login.searchParams.set("error", error.message);
      return NextResponse.redirect(login);
    }
  }
  return NextResponse.redirect(new URL(next, url.origin));
}
