# GrokRxiv — production deployment runbook

This is the operator-facing deployment guide for GrokRxiv. It covers what runs
where, every required environment variable, day-2 ops, and disaster
recovery. It assumes you have already run the M1 smoke test
(`tests/m1-pipeline.sh`) locally and got 8/8.

## Topology at a glance

```
                ┌──────────────────────┐
   browser ──▶  │ Vercel (app web)     │  Next.js 16 (App Router, Turbopack)
                │  /                   │  Anon Supabase JS reads
                │  /reviews            │  Service-token write proxies
                │  /reviews/<id>       │
                │  /papers/<arxiv_id>  │
                │  /api/v1/*           │  (auth + proxy → Rust)
                └─────────┬────────────┘
                        │   private network / proxy
                          ▼
                ┌──────────────────────┐
                │ Railway (Rust)       │  axum + tokio supervisor
                │  orchestrator + cli  │  ingest_pipeline, 6-agent DAG
                │  /healthz            │  GitHub PR opener, webhook sink
                │  /webhook/github     │
                │  /internal/v1/*      │
                └─────────┬────────────┘
                          │ sqlx
                          ▼
                ┌──────────────────────┐
                │ Supabase Cloud       │  Postgres 15, Storage,
                │  - papers            │  RLS-gated reads, service-role writes
                │  - reviews           │
                │  - paper_assets      │
                │  - moderation_queue  │
                │  + 6 storage buckets │
                └──────────────────────┘

   GitHub mirror: GrokRxiv/grokrxiv-reviews  ←  PRs opened by Railway
                                              merge webhook → Railway
```

The operator CLI is `agh`; GrokRxiv runs as an installed DAGOps app behind
`agh app run grokrxiv ...`. The app runtime binary is used by the orchestrator
adapter and deployment containers, not as the end-user product surface.

## Component 1 — Orchestrator on Railway

### Build

* `agenthero/apps/grokrxiv/infra/railway.json` points Railway at
  `agenthero/apps/grokrxiv/infra/docker/orchestrator.Dockerfile`.
* The Dockerfile is a 2-stage musl-friendly build: `rust:1.82-slim` for the
  release binary, `debian:bookworm-slim` for the runtime image. The image
  bakes official Pandoc plus the app manifest, DAGs, agents, schemas, and
  prompts into `/etc/agenthero/apps/grokrxiv`, so an app contract change
  requires a fresh build. Set Docker build arg `INSTALL_PANDOC=0` only for a
  slim image that will provide `pandoc` separately via PATH or
  `GROKRXIV_PANDOC_BIN`.
* Start command (from `railway.json`): `/usr/local/bin/orchestrator`.
* Health check path: `/healthz` (handled in `crates/orchestrator/src/routes/healthz.rs`).
* Restart policy: ON_FAILURE up to 5 retries.

### Required environment variables

Set these in Railway → service → Variables. The orchestrator refuses to start
without `DATABASE_URL`; everything else degrades to a stub or simulated mode
if absent (see `crates/orchestrator/src/config.rs`).

| Variable                          | Purpose                                                    |
|-----------------------------------|------------------------------------------------------------|
| `DATABASE_URL`                    | `postgresql://...` to Supabase Postgres                    |
| `SUPABASE_URL`                    | e.g. `https://<project-ref>.supabase.co`                   |
| `SUPABASE_SERVICE_ROLE_KEY`       | Server-only key. NEVER ship to the web tier.               |
| `GITHUB_TOKEN`                    | PAT (scope `public_repo`) for the moderation repo          |
| `GITHUB_WEBHOOK_SECRET`           | HMAC secret matching the GitHub webhook setting            |
| `GROKRXIV_REVIEWS_OWNER`          | Default `GrokRxiv`                                         |
| `GROKRXIV_REVIEWS_REPO`           | Default `grokrxiv-reviews`                                 |
| `ANTHROPIC_API_KEY`               | Claude (Anthropic) project key — NOT a personal CLI key    |
| `OPENAI_API_KEY`                  | OpenAI key                                                 |
| `GOOGLE_GENERATIVE_AI_API_KEY`    | Gemini key                                                 |
| `VLLM_BASE_URL`                   | Optional vLLM endpoint                                     |
| `GROKRXIV_DATA_REPO_PATH`         | Path to the `grokrxiv-data` Git checkout in the container  |
| `GROKRXIV_DATA_REPO_REMOTE`       | `git@github.com:GrokRxiv/grokrxiv-data.git`                |
| `WEB_REVALIDATE_URL`              | `https://<vercel-domain>/api/revalidate` (web revalidate)  |
| `REVALIDATE_SECRET`               | Bearer the webhook posts to `WEB_REVALIDATE_URL`           |
| `ORCHESTRATOR_BIND`               | Default `0.0.0.0:8080`; Railway maps to its public port    |
| `ARXIV_USER_AGENT`                | RFC-conformant user agent, e.g. `GrokRxiv/0.1 (+contact)`  |
| `GROKRXIV_PREVIEW_MODEL`          | Controls landing-page previews                             |
| `RUST_LOG`                        | `info,grokrxiv_app_runtime=info,agenthero_orchestrator=info` |

### Optional / niche variables

| Variable                          | Purpose                                                    |
|-----------------------------------|------------------------------------------------------------|
| `AGENTHERO_RUNNER`                | `api` / `cli`                                               |
| `AGENTHERO_EXTRACTOR`             | `cli` / `api` staged extraction backend                    |
| `AGENTHERO_MAX_COST_USD`          | Hard ceiling per review run                                |
| `AGENTHERO_APPS_ROOT`             | Defaults to the image-baked `/etc/agenthero/apps`          |
| `AGENTHERO_AGENTS_DIR`            | Defaults to `/etc/agenthero/apps/grokrxiv/agents`          |
| `AGENTHERO_DAGS_DIR`              | Defaults to `/etc/agenthero/apps/grokrxiv/dags`            |
| `GROKRXIV_NO_CACHE`               | Set `1` to force-bust the FP6 review cache                 |
| `ADMIN_TOKEN`                     | Bearer for the `/ingest` admin endpoint                    |

### Reachable check

After `railway up` finishes, hit:

```sh
curl -sf "https://<railway-domain>/healthz"   # expect 200 + JSON ok
curl -sf "https://<railway-domain>/version"   # expect git sha + build time
```

## Component 2 — Web tier on Vercel

### Build

* The `agenthero/apps/grokrxiv/web` workspace is a vanilla Next.js 16 app (App
  Router, Turbopack, Tailwind 4). Vercel auto-detects via `pnpm` and
  `package.json`. No `vercel.json` is required.
* The Vercel project root must be `agenthero/apps/grokrxiv/web`.
* Output is statically rendered for `/`, `/about`, `/api-docs`, `/legal`,
  `/reviews`, and dynamically rendered for `/reviews/<id>` and `/papers/<id>`
  with ISR via the `/api/revalidate` route.

### Required environment variables

In the Vercel project Settings → Environment Variables, scope to `Production`
+ `Preview`:

| Variable                            | Purpose                                                  |
|-------------------------------------|----------------------------------------------------------|
| `NEXT_PUBLIC_SUPABASE_URL`          | Supabase project URL (public — anon reads only)          |
| `NEXT_PUBLIC_SUPABASE_ANON_KEY`     | Supabase anon key (public — RLS-gated)                   |
| `SUPABASE_SERVICE_ROLE_KEY`         | Server-only key for server routes                        |
| `AGENTHERO_SERVICE_TOKEN`           | Bearer the web sends to the Rust orchestrator            |
| `ORCHESTRATOR_INTERNAL_URL`         | e.g. `https://<railway-domain>` — Rust API base          |
| `REVALIDATE_SECRET`                 | Bearer required by `/api/revalidate`                     |
| `NEXT_PUBLIC_SITE_URL`              | e.g. `https://grokrxiv.org`                              |
| `GROKRXIV_PUBLIC_URL`               | Canonical URL for OG / sitemap / JSON-LD                 |
| `GROKRXIV_BILLING_ENABLED`          | Set `1` only when Stripe is configured                   |
| `STRIPE_SECRET_KEY`                 | Required when billing is enabled                         |
| `STRIPE_WEBHOOK_SECRET`             | Required when billing is enabled                         |
| `STRIPE_SUPPORTER_PRICE_ID`         | Required when billing is enabled                         |
| `STRIPE_RESEARCHER_PRICE_ID`        | Required when billing is enabled                         |
| `GROKRXIV_SUPER_ADMIN_EMAIL`        | Optional admin seed/default account                      |
| `GROKRXIV_FREE_REVIEW_LIMIT`        | Free review quota shown in dashboard; default `3`        |

Note: in the FP-RPT3c plan the orchestrator-reach env was called
`INTERNAL_RUST_URL`. The actual name shipped in `agenthero/apps/grokrxiv/web/lib/env.ts` is
`ORCHESTRATOR_INTERNAL_URL`. Set BOTH if you want belt-and-suspenders during
the cut-over — the env-reader only consults the latter today.

### DNS / TLS

* Vercel auto-issues TLS for the assigned `<project>.vercel.app` and any
  custom domain you point at it. Set the apex (`grokrxiv.org`) and `www`
  CNAMEs per the Vercel UI.
* The Railway service ships with a `<service>.up.railway.app` hostname by
  default; assign a custom domain (e.g. `api.grokrxiv.org`) via Railway
  Settings → Domains.

## Component 3 — Supabase Cloud (Postgres + Storage)

### Setup

1. Create a new Supabase Cloud project. Region should be close to the Railway
   region (us-east-1 / iad-us most often).
2. Copy `Project URL` → `SUPABASE_URL` on Railway. Copy
   `Project URL` again → `NEXT_PUBLIC_SUPABASE_URL` on Vercel.
3. Copy `Project anon key` → `NEXT_PUBLIC_SUPABASE_ANON_KEY` on Vercel.
4. Copy `Project service role key` → `SUPABASE_SERVICE_ROLE_KEY` on Railway.
   This MUST NEVER appear on the Vercel side; it bypasses RLS.

### Applying the schema

Migrations live in `migrations/` (Postgres SQL) and `supabase/migrations/`
(Supabase-CLI-shaped duplicates for `supabase db push`). To apply:

```sh
# Locally (recommended dry-run path):
supabase db push --dry-run     # check planned diff
supabase db push               # apply against linked cloud project
```

For ad-hoc fixes from your laptop:

```sh
psql "$DATABASE_URL" < migrations/20260516000004_raw_pdfs_rls_tighten.sql
```

### Storage buckets

Buckets are created in `migrations/20260516000002_paper_assets_bucket.sql`:
`raw-pdfs`, `raw-source`, `extracted-markdown`, `extracted-json`,
`embeddings`, `review-artifacts`. All are private (`public=false`); anon SELECT
is granted only where the per-bucket RLS allows.

### Verify RLS post-deploy

The `raw-pdfs` bucket is the most sensitive — it must NOT leak PDFs of
unpublished reviews. Verify with the curl from the FP-RPT3c C1 check:

```sh
curl -sf -o /dev/null -w "%{http_code}\n" \
  -H "apikey: $NEXT_PUBLIC_SUPABASE_ANON_KEY" \
  "$SUPABASE_URL/storage/v1/object/raw-pdfs/2605.99999.pdf"
# Expect 4xx for any arXiv id without a corresponding `published`/`corrected`
# review. Only after a real review is published does this return 200.
```

If you ever see a 200 for an unpublished id, the migration didn't land —
re-apply `20260516000004_raw_pdfs_rls_tighten.sql` and re-curl.

## Component 4 — GitHub moderation repo

* Repo: `GrokRxiv/grokrxiv-reviews` (already exists; do not re-create).
* PAT scope: `public_repo` (read + write on public repos). NOT `repo` — the
  publisher does not need access to private repos and overprivileged tokens
  are an audit risk.
* Webhook:
  * URL: `https://<railway-domain>/webhook/github`
  * Content type: `application/json`
  * Secret: matches `GITHUB_WEBHOOK_SECRET` on Railway
  * Events: only `Pull request` (we filter to `closed + merged` in the handler)

The webhook handler (`crates/orchestrator/src/routes/webhook.rs`) flips the
review row to `status='published'` only after the human merge. This is the
human-moderation gate documented on `/about`.

## Day-2 ops

### Apply a new migration

Local Supabase (dev):
```sh
docker exec -i supabase_db_grokrxiv psql -U postgres -d postgres \
  < migrations/<NEW_FILE>.sql
```

Supabase Cloud (managed):
```sh
supabase link --project-ref <ref>     # one-time
supabase db push                      # applies anything under supabase/migrations/
```

If a migration only lives under `migrations/` (raw Postgres) and not under
`supabase/migrations/`, mirror it before pushing.

### Rotate API keys

1. Regenerate the key in the provider console (Anthropic / OpenAI / Google).
2. Update the matching Railway env var (`ANTHROPIC_API_KEY`, etc).
3. Trigger a redeploy from Railway → Service → Deploy.
4. If the key is also used by the web tier (it should NOT be — keys live
   server-side only), update Vercel and redeploy.
5. Revoke the old key.

### Rotate the GitHub PAT

1. Generate a new PAT with scope `public_repo`.
2. Replace `GITHUB_TOKEN` on Railway and redeploy.
3. Revoke the old PAT.

### Rotate the Supabase service-role key

1. Supabase project → Settings → API → Roll service role key.
2. Update `SUPABASE_SERVICE_ROLE_KEY` on Railway and redeploy.
3. The anon key is rotated separately; if you roll it, also update
   `NEXT_PUBLIC_SUPABASE_ANON_KEY` on Vercel and redeploy.

### View logs

* Railway: Service → Deployments → click the active deploy → Logs. Filter on
  `tracing` levels via `RUST_LOG`.
* Vercel: Project → Deployments → click deploy → Function Logs / Build Logs.
* Supabase: Project → Logs → choose `Postgres`, `Storage`, or `API`. Studio
  also exposes table contents (set `Project → Settings → API → Schema`).

### Force a re-extraction of a paper

```sh
grokrxiv ingest <arxiv_id> --no-cache
```

`--no-cache` bypasses the FP6 `review_cache` and re-runs every specialist.
Use this when an upstream schema or model change should produce a different
review for an already-ingested paper. The orchestrator will auto-supersede
the prior active review (status=`withdrawn`), close its open PR on the
moderation repo (FP-RPT3c C2), and open a new PR.

### Trigger publication

After ingestion, the review sits in `status='awaiting_moderation'`. To open
the moderation PR:

```sh
grokrxiv approve <review_id>
```

The review then transitions to `pr_open`. It only flips to `published` after
a human moderator merges the PR on GitHub and the merge webhook fires back at
Railway.

### Manually withdraw a review

```sh
grokrxiv withdraw <review_id> --reason "duplicate / author dispute / ..."
```

## Disaster recovery

### Postgres data loss / corruption

* Supabase Cloud takes automatic daily backups with PITR at the Pro plan and
  above. The recovery path is: Project → Backups → restore to point in time.
* `papers` and `reviews` are the only tables whose loss is "real" data loss —
  the rest (`review_agents`, `review_inputs`, `review_cache`) can be
  regenerated by re-running ingest.
* `paper_assets` rows are pointers; the underlying files in Supabase Storage
  survive a Postgres rollback. After restore, verify the
  `paper_assets.storage_prefix` → `storage.objects.name` join still resolves
  (`SELECT paper_id, storage_prefix FROM paper_assets LIMIT 5;`).

### Storage object loss

The source-of-truth is the Tier-1 Git mirror at `GrokRxiv/grokrxiv-data`. To
recover a single bucket from scratch:

```sh
# 1. Mirror grokrxiv-data locally.
git clone git@github.com:GrokRxiv/grokrxiv-data.git
# 2. Re-upload PDFs using the service-role key.
for f in grokrxiv-data/papers/*/original.pdf; do
  id=$(basename $(dirname "$f"))
  curl -X POST \
    -H "Authorization: Bearer $SUPABASE_SERVICE_ROLE_KEY" \
    -H "Content-Type: application/pdf" \
    --data-binary "@$f" \
    "$SUPABASE_URL/storage/v1/object/raw-pdfs/$id.pdf"
done
```

### Stuck moderation queue

If `moderation_queue` rows accumulate with no GitHub merge, check:
1. `grokrxiv approve` was actually called (status=`pr_open`).
2. The GitHub webhook is reaching Railway (`/webhook/github`).
3. `GITHUB_WEBHOOK_SECRET` on Railway matches the one set on the GitHub repo.
4. The merged branch matches `review/<arxiv_id>-<short>` — the webhook handler
   rejects anything else.

## Known TBDs

* `infra/railway.json` does not currently encode a healthcheck for the
  Postgres or Supabase dependencies — Railway runs the orchestrator
  unconditionally. Add a `wait-for-postgres` step in the Dockerfile if
  cold-start ordering becomes an issue.
* `INTERNAL_RUST_URL` (mentioned in some earlier plans) is NOT a real env
  var; use `ORCHESTRATOR_INTERNAL_URL` instead. The plan name is kept here
  for searchability.
* `GROKRXIV_DATA_REPO_PATH` on Railway needs a writable mount; the default
  Dockerfile does not provide one. Either mount a Railway volume at
  `/var/grokrxiv-data` and set the env, OR run extraction with
  `GROKRXIV_DRY_RUN_STORAGE=1` to skip the Git push step.
