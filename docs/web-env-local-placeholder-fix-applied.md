# apps/web/.env.local — replaced placeholder Supabase keys with the real ones (2026-05-15)

## What

`apps/web/.env.local` used to contain literal `placeholder-anon-key-for-dev` and `placeholder-service-role-key`. Replaced with the real local-Supabase keys from `supabase status --output env`.

## Why

Symptom during RPT1 Phase 6.5: `/api/v1/papers/<arxiv>` returned `{"error":"not_found"}`, review pages rendered `<title>Review not found · GrokRxiv</title>`, homepage grid skipped the 3 new published reviews — even though direct curl to `http://127.0.0.1:54321/rest/v1/papers?arxiv_id=eq.2605.00403` (with the real anon key) returned the row immediately.

Diagnosis: Next.js loads `.env.local` AFTER `.env`. The placeholder anon key in `.env.local` was overriding the real key in `.env`. Every Supabase request from the web app went out with an invalid JWT and Supabase silently returned empty result sets (PostgREST's behavior when the JWT is malformed but not unauthorized).

## How

| Change | Location |
|---|---|
| Real `NEXT_PUBLIC_SUPABASE_ANON_KEY` (sourced from `supabase status`) | `apps/web/.env.local` |
| Real `SUPABASE_SERVICE_ROLE_KEY` (sourced from `supabase status`) | `apps/web/.env.local` |
| Supabase URL normalized to `http://127.0.0.1:54321` (matches the running Docker stack) | `apps/web/.env.local` |

`.env.local` is gitignored, so this is a per-machine setup step that needs to happen once after `supabase start` produces keys.

## Risk

| Risk | Mitigation |
|---|---|
| Keys are committed accidentally | `.env.local` is in `.gitignore` (FP6 Track D added the rule); `git status` confirms it's not tracked |
| Real keys leak via the browser via `NEXT_PUBLIC_*` prefix | Acceptable — the anon key is supposed to be public; that's the point. Service-role key is server-only (no `NEXT_PUBLIC_` prefix) so it never reaches the client bundle. |
| Local key rotation by re-running `supabase start` after a long pause | Re-run `supabase status --output env | grep ANON_KEY` and re-copy. Documented inline in this doc. |

## Reversal

`.env.local` is gitignored; recovery is trivial:

```sh
# Re-derive keys from the running stack:
REAL_ANON=$(supabase status --output env | grep ANON_KEY | cut -d= -f2 | tr -d '"')
REAL_SERVICE=$(supabase status --output env | grep SERVICE_ROLE_KEY | cut -d= -f2 | tr -d '"')
sed -i.bak "s|^NEXT_PUBLIC_SUPABASE_ANON_KEY=.*|NEXT_PUBLIC_SUPABASE_ANON_KEY=$REAL_ANON|" apps/web/.env.local
sed -i.bak "s|^SUPABASE_SERVICE_ROLE_KEY=.*|SUPABASE_SERVICE_ROLE_KEY=$REAL_SERVICE|" apps/web/.env.local
```

## Verification

```sh
cd apps/web && pnpm dev
# In another shell:
curl -s http://localhost:3000/api/v1/papers/2605.00403 | head -c 200
# Should return real paper JSON, not {"error":"not_found"}.
```
