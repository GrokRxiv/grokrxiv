"use client";

import { createBrowserClient } from "@supabase/ssr";
import { SUPABASE_ANON_KEY, SUPABASE_BROWSER_URL } from "@/lib/env-public";
import { SUPABASE_AUTH_COOKIE_NAME } from "@/lib/supabase/cookie";

export function createSupabaseBrowserClient() {
  return createBrowserClient(SUPABASE_BROWSER_URL, SUPABASE_ANON_KEY, {
    cookieOptions: { name: SUPABASE_AUTH_COOKIE_NAME },
  });
}
