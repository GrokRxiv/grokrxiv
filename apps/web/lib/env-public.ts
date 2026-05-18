"use client";

export const SUPABASE_BROWSER_URL =
  process.env.NEXT_PUBLIC_SUPABASE_URL ?? "";
export const SUPABASE_ANON_KEY =
  process.env.NEXT_PUBLIC_SUPABASE_ANON_KEY ?? "";

export function isSupabaseBrowserConfigured(): boolean {
  return SUPABASE_BROWSER_URL.length > 0 && SUPABASE_ANON_KEY.length > 0;
}
