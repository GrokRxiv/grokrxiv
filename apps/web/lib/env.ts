// Centralized env access with safe defaults.

export const SITE_URL =
  process.env.NEXT_PUBLIC_SITE_URL ?? "http://localhost:3000";

// Canonical URL used for SEO surfaces (JSON-LD, canonical, sitemap, OG).
// We always advertise the production hostname so reviews are crawled and
// indexed under the public domain even when rendered from a staging deploy or
// from a local dev preview. Override via `GROKRXIV_PUBLIC_URL` only when an
// alternate canonical is genuinely required.
export const CANONICAL_URL =
  process.env.GROKRXIV_PUBLIC_URL ?? "https://grokrxiv.org";

export const ORCHESTRATOR_INTERNAL_URL =
  process.env.ORCHESTRATOR_INTERNAL_URL ?? "http://localhost:8080";

export const SUPABASE_URL = process.env.NEXT_PUBLIC_SUPABASE_URL ?? "";
export const SUPABASE_ANON_KEY =
  process.env.NEXT_PUBLIC_SUPABASE_ANON_KEY ?? "";

export const REVALIDATE_SECRET = process.env.REVALIDATE_SECRET ?? "";

export function isSupabaseConfigured(): boolean {
  return SUPABASE_URL.length > 0 && SUPABASE_ANON_KEY.length > 0;
}
