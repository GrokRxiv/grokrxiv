# GrokRxiv Vercel Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deploy the GrokRxiv Next.js website to Vercel and verify review `a157d4e6-d47d-4ff4-95d2-0d2663ab179d` for arXiv `2606.23240` is visible outside localhost.

**Architecture:** The web app stays owned by `agenthero/apps/grokrxiv/web` and reads public review data from Supabase using anon/server clients. Vercel serves the Next.js app, `grokrxiv.org` and `www.grokrxiv.org` point to that Vercel project, and the GrokRxiv publish flow invalidates Vercel caches via `/api/revalidate`.

**Tech Stack:** Next.js 16, React 19, pnpm workspace, Supabase, Vercel CLI/Git integration, GrokRxiv `agh app run grokrxiv ...` publish pipeline.

## Global Constraints

- Do not commit Vercel tokens, Supabase service keys, revalidate secrets, Stripe secrets, or `.vercel/project.json`.
- Production Vercel cannot read `http://127.0.0.1:54321`; use a remotely reachable Supabase project or a public database endpoint.
- Production canonical URLs are `https://grokrxiv.org`.
- Public anonymous review pages only show statuses in `pr_open`, `published`, `corrected`, or `rejected`.
- Review `a157d4e6-d47d-4ff4-95d2-0d2663ab179d` is currently `pr_open`, `public`, and should render once the deployed app points at a database containing that row.
- Current production `https://grokrxiv.org/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d` returns Squarespace "Coming Soon"; DNS/domain configuration must change.

---

## Files And Surfaces

- Modify: `agenthero/apps/grokrxiv/app.yaml`
  - Keep the declared Vercel surface in sync with actual Vercel project settings.
- Modify: `docs/DEPLOYMENT.md`
  - Record the exact Vercel root, build command, environment variables, DNS, and revalidate steps.
- Optional create: `.github/workflows/grokrxiv-web-vercel.yml`
  - Only if we choose explicit GitHub Actions deployment instead of Vercel Git integration.
- Local generated, do not commit: `agenthero/apps/grokrxiv/web/.vercel/project.json`
  - Created by `vercel link`; contains project linkage, not source configuration.

## Task 1: Verify Local Web Baseline

**Files:**
- No source changes.

**Interfaces:**
- Consumes: local Supabase data, local `agenthero/apps/grokrxiv/web/.env.local`.
- Produces: evidence that the web code can render the target review before deployment.

- [ ] **Step 1: Confirm target review is public-renderable in the database**

Run:

```bash
set -a
. ./.env
for f in $(printf '%s' "$AGENTHERO_ENV_FILES" | tr ',' ' '); do [ -f "$f" ] && . "$f"; done
set +a
psql "$DATABASE_URL" -Atc "select r.id, r.status, r.visibility, p.source_id, p.title from reviews r join papers p on p.id = r.paper_id where r.id = 'a157d4e6-d47d-4ff4-95d2-0d2663ab179d';"
```

Expected:

```text
a157d4e6-d47d-4ff4-95d2-0d2663ab179d|pr_open|public|2606.23240|A conjecture on the action of Hecke operators
```

- [ ] **Step 2: Build the web app locally**

Run:

```bash
pnpm install --frozen-lockfile
pnpm --filter @grokrxiv/web typecheck
pnpm --filter @grokrxiv/web build
```

Expected: `typecheck` and `build` exit `0`.

- [ ] **Step 3: Start local web**

Run:

```bash
cd agenthero/apps/grokrxiv/web
pnpm dev
```

Expected: Next.js serves on `http://localhost:3000`.

- [ ] **Step 4: Verify the review route locally**

Run in another shell:

```bash
curl -fsSL http://localhost:3000/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d \
  | rg "A conjecture on the action of Hecke operators|Publication record|Meta-review"
```

Expected: all three strings are found.

## Task 2: Provision Production Data Access

**Files:**
- No source changes unless a new env example is needed after discovery.

**Interfaces:**
- Consumes: production Supabase project credentials.
- Produces: Vercel-readable database containing the target review.

- [ ] **Step 1: Decide the production Supabase source**

Use one of these two paths:

```text
Preferred: existing Supabase Cloud project that already has GrokRxiv migrations.
Fallback: create/link a new Supabase Cloud project and apply migrations before importing data.
```

Do not deploy Vercel against the current localhost Supabase URL.

- [ ] **Step 2: Apply GrokRxiv migrations to the production database if needed**

Run after setting `SUPABASE_PROJECT_REF` and logging in:

```bash
supabase link --project-ref "$SUPABASE_PROJECT_REF"
supabase db push
```

Expected: migrations complete without errors.

- [ ] **Step 3: Move the target review data into production if it is only local**

Preferred operational path: rerun the GrokRxiv review against the production database after Vercel is configured, with cache disabled and Lean disabled:

```bash
GROKRXIV_NO_CACHE=1 \
AGENTHERO_ANTIGRAVITY_BIN="$(command -v agy)" \
DATABASE_URL="$PRODUCTION_DATABASE_URL" \
agh app run grokrxiv review 2606.23240 --type arxiv --no-lean
```

Expected: the resulting review row has `visibility='public'` and status `pr_open`, `published`, `corrected`, or `rejected`.

If rerunning would create an unwanted duplicate PR, export/import only the required local rows into production:

```bash
mkdir -p /tmp/grokrxiv-review-a157
psql "$DATABASE_URL" -c "\copy (select * from papers where id = '60ebb9bc-718b-4aea-811a-1683bd3c2eba') to '/tmp/grokrxiv-review-a157/papers.csv' csv header"
psql "$DATABASE_URL" -c "\copy (select * from reviews where id = 'a157d4e6-d47d-4ff4-95d2-0d2663ab179d') to '/tmp/grokrxiv-review-a157/reviews.csv' csv header"
psql "$DATABASE_URL" -c "\copy (select * from review_agents where review_id = 'a157d4e6-d47d-4ff4-95d2-0d2663ab179d') to '/tmp/grokrxiv-review-a157/review_agents.csv' csv header"
psql "$DATABASE_URL" -c "\copy (select * from review_gate_failures where review_id = 'a157d4e6-d47d-4ff4-95d2-0d2663ab179d') to '/tmp/grokrxiv-review-a157/review_gate_failures.csv' csv header"
```

Then import into production after confirming schemas match:

```bash
psql "$PRODUCTION_DATABASE_URL" -c "\copy papers from '/tmp/grokrxiv-review-a157/papers.csv' csv header"
psql "$PRODUCTION_DATABASE_URL" -c "\copy reviews from '/tmp/grokrxiv-review-a157/reviews.csv' csv header"
psql "$PRODUCTION_DATABASE_URL" -c "\copy review_agents from '/tmp/grokrxiv-review-a157/review_agents.csv' csv header"
psql "$PRODUCTION_DATABASE_URL" -c "\copy review_gate_failures from '/tmp/grokrxiv-review-a157/review_gate_failures.csv' csv header"
```

Expected: production query returns the target review row.

## Task 3: Link The Vercel Project

**Files:**
- Local generated: `agenthero/apps/grokrxiv/web/.vercel/project.json` (do not commit).

**Interfaces:**
- Consumes: Vercel account/team access.
- Produces: linked Vercel project `grokrxiv`.

- [ ] **Step 1: Install or verify Vercel CLI**

Run:

```bash
vercel --version || npm i -g vercel
```

Expected: a Vercel CLI version is printed.

- [ ] **Step 2: Link from the Next.js project root**

Run:

```bash
cd agenthero/apps/grokrxiv/web
vercel link --project grokrxiv
```

Expected: `.vercel/project.json` is created locally and `vercel project ls` shows `grokrxiv`.

- [ ] **Step 3: Configure Vercel project settings**

Set these in the Vercel dashboard if using Git integration:

```text
Framework Preset: Next.js
Root Directory: agenthero/apps/grokrxiv/web
Install Command: pnpm install --frozen-lockfile
Build Command: pnpm build
Output Directory: .next
Node.js Version: 22.x, or the latest Vercel-supported Node version satisfying >=20.11
```

Expected: Vercel builds from the web package, not from the repo root.

## Task 4: Configure Vercel Environment Variables

**Files:**
- No source changes.

**Interfaces:**
- Consumes: Supabase production URL/keys, revalidate secret, optional orchestrator and Stripe settings.
- Produces: production and preview Vercel env vars.

- [ ] **Step 1: Add required read/render environment variables**

Run from `agenthero/apps/grokrxiv/web`:

```bash
vercel env add NEXT_PUBLIC_SITE_URL production
vercel env add GROKRXIV_PUBLIC_URL production
vercel env add NEXT_PUBLIC_SUPABASE_URL production
vercel env add NEXT_PUBLIC_SUPABASE_ANON_KEY production
vercel env add SUPABASE_SERVICE_ROLE_KEY production
vercel env add REVALIDATE_SECRET production
```

Use these values:

```text
NEXT_PUBLIC_SITE_URL=https://grokrxiv.org
GROKRXIV_PUBLIC_URL=https://grokrxiv.org
NEXT_PUBLIC_SUPABASE_URL=https://<supabase-project-ref>.supabase.co
NEXT_PUBLIC_SUPABASE_ANON_KEY=<production anon key>
SUPABASE_SERVICE_ROLE_KEY=<production service role key>
REVALIDATE_SECRET=<random 32+ byte secret>
```

- [ ] **Step 2: Add optional operational environment variables**

Run:

```bash
vercel env add ORCHESTRATOR_INTERNAL_URL production
vercel env add AGENTHERO_SERVICE_TOKEN production
vercel env add GROKRXIV_BILLING_ENABLED production
vercel env add GROKRXIV_FREE_REVIEW_LIMIT production
vercel env add GROKRXIV_SUPER_ADMIN_EMAIL production
```

Use these values unless production orchestrator/billing is ready:

```text
ORCHESTRATOR_INTERNAL_URL=
AGENTHERO_SERVICE_TOKEN=
GROKRXIV_BILLING_ENABLED=0
GROKRXIV_FREE_REVIEW_LIMIT=3
GROKRXIV_SUPER_ADMIN_EMAIL=<operator admin email>
```

- [ ] **Step 3: Add Stripe env vars only if billing is enabled**

If `GROKRXIV_BILLING_ENABLED=1`, run:

```bash
vercel env add STRIPE_SECRET_KEY production
vercel env add STRIPE_WEBHOOK_SECRET production
vercel env add STRIPE_SUPPORTER_PRICE_ID production
vercel env add STRIPE_RESEARCHER_PRICE_ID production
```

Expected: `vercel env ls production` shows every variable name without exposing values.

## Task 5: Reconcile The App Deployment Manifest

**Files:**
- Modify: `agenthero/apps/grokrxiv/app.yaml`
- Modify: `docs/DEPLOYMENT.md`

**Interfaces:**
- Consumes: Vercel project settings from Task 3.
- Produces: repository docs/config that match the deployed project.

- [ ] **Step 1: Update the manifest build command**

Change `agenthero/apps/grokrxiv/app.yaml` deployment entry:

```yaml
deployments:
  - kind: vercel
    id: web
    project: grokrxiv
    root: web
    framework: nextjs
    build_command: pnpm build
    output_directory: .next
```

Expected: the app-relative `root: web` matches Vercel Root Directory `agenthero/apps/grokrxiv/web`, so the build command runs in the web package.

- [ ] **Step 2: Document the production env contract**

Add a section to `docs/DEPLOYMENT.md` with:

```markdown
### GrokRxiv Web on Vercel

- Vercel project: `grokrxiv`
- Root directory: `agenthero/apps/grokrxiv/web`
- Build command: `pnpm build`
- Output directory: `.next`
- Canonical URL: `https://grokrxiv.org`
- Revalidate endpoint: `https://grokrxiv.org/api/revalidate`
- Required production env vars: `NEXT_PUBLIC_SITE_URL`, `GROKRXIV_PUBLIC_URL`, `NEXT_PUBLIC_SUPABASE_URL`, `NEXT_PUBLIC_SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_ROLE_KEY`, `REVALIDATE_SECRET`
```

- [ ] **Step 3: Verify source changes**

Run:

```bash
git diff -- agenthero/apps/grokrxiv/app.yaml docs/DEPLOYMENT.md
git diff --check
```

Expected: diff shows only the manifest/doc updates and `git diff --check` exits `0`.

## Task 6: Deploy And Verify A Preview

**Files:**
- No source changes.

**Interfaces:**
- Consumes: linked Vercel project and production env vars.
- Produces: preview URL that renders the target review.

- [ ] **Step 1: Pull production env locally for a production-equivalent build**

Run:

```bash
cd agenthero/apps/grokrxiv/web
vercel pull --yes --environment=production
```

Expected: Vercel writes local build env files under `.vercel/`.

- [ ] **Step 2: Build using Vercel**

Run:

```bash
vercel build --prod
```

Expected: build exits `0` and writes `.vercel/output`.

- [ ] **Step 3: Deploy prebuilt preview**

Run:

```bash
PREVIEW_URL="$(vercel deploy --prebuilt)"
printf '%s\n' "$PREVIEW_URL"
```

Expected: `PREVIEW_URL` is an `https://...vercel.app` URL.

- [ ] **Step 4: Verify preview API and page**

Run:

```bash
curl -fsSL "$PREVIEW_URL/api/v1/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d" \
  | jq -e '.id == "a157d4e6-d47d-4ff4-95d2-0d2663ab179d" and .status == "pr_open"'

curl -fsSL "$PREVIEW_URL/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d" \
  | rg "A conjecture on the action of Hecke operators|Publication record|Meta-review"
```

Expected: `jq` exits `0`, and the page contains the review title and review sections.

## Task 7: Promote To Production And Attach Domains

**Files:**
- No source changes.

**Interfaces:**
- Consumes: validated preview deployment and Vercel domain ownership.
- Produces: production app on `https://grokrxiv.org`.

- [ ] **Step 1: Promote the validated preview**

Run:

```bash
vercel promote "$PREVIEW_URL"
```

Expected: Vercel reports the deployment promoted to production.

- [ ] **Step 2: Add domains to the Vercel project**

Run:

```bash
vercel domains add grokrxiv.org
vercel domains add www.grokrxiv.org
vercel domains inspect grokrxiv.org
vercel domains inspect www.grokrxiv.org
```

Expected: Vercel returns the required DNS records or confirms valid configuration.

- [ ] **Step 3: Move DNS away from Squarespace**

At the DNS host, replace the Squarespace records for `grokrxiv.org` and `www.grokrxiv.org` with the records Vercel reports.

Expected:

```bash
dig +short grokrxiv.org
dig +short www.grokrxiv.org
curl -fsSL -D /tmp/grokrxiv-prod.headers https://grokrxiv.org/ -o /tmp/grokrxiv-prod.html
rg -n "server: Vercel|x-vercel" /tmp/grokrxiv-prod.headers
```

The headers should indicate Vercel, not Squarespace.

## Task 8: Wire Revalidation Back Into GrokRxiv Publishing

**Files:**
- Modify only env files or secret stores, not source code.

**Interfaces:**
- Consumes: Vercel production URL and `REVALIDATE_SECRET`.
- Produces: future GrokRxiv reviews invalidate the production site cache.

- [ ] **Step 1: Set GrokRxiv publish env**

Set the runtime env used by `agh app run grokrxiv ...`:

```bash
WEB_REVALIDATE_URL=https://grokrxiv.org/api/revalidate
REVALIDATE_SECRET=<same value as Vercel REVALIDATE_SECRET>
GROKRXIV_PUBLIC_URL=https://grokrxiv.org
NEXT_PUBLIC_SITE_URL=https://grokrxiv.org
```

Expected: `agh app run grokrxiv review ...` posts revalidation to the public Vercel app after PR/status changes.

- [ ] **Step 2: Revalidate the target review**

Run:

```bash
curl -fsSL -X POST https://grokrxiv.org/api/revalidate \
  -H "content-type: application/json" \
  -H "x-revalidate-secret: $REVALIDATE_SECRET" \
  -d '{"review_id":"a157d4e6-d47d-4ff4-95d2-0d2663ab179d","arxiv_id":"2606.23240","paths":["/","/reviews","/papers/2606.23240"]}' \
  | jq .
```

Expected:

```json
{
  "ok": true
}
```

## Task 9: Final Acceptance Checks

**Files:**
- No source changes.

**Interfaces:**
- Consumes: production Vercel deployment and production Supabase data.
- Produces: evidence that the review is visible on the website.

- [ ] **Step 1: Verify production review API**

Run:

```bash
curl -fsSL https://grokrxiv.org/api/v1/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d \
  | jq -e '.id == "a157d4e6-d47d-4ff4-95d2-0d2663ab179d" and (.status == "pr_open" or .status == "published" or .status == "corrected" or .status == "rejected")'
```

Expected: `jq` exits `0`.

- [ ] **Step 2: Verify production HTML**

Run:

```bash
curl -fsSL -D /tmp/grokrxiv-review-prod.headers \
  https://grokrxiv.org/reviews/a157d4e6-d47d-4ff4-95d2-0d2663ab179d \
  -o /tmp/grokrxiv-review-prod.html

rg -n "server: Vercel|x-vercel" /tmp/grokrxiv-review-prod.headers
rg -n "A conjecture on the action of Hecke operators|Publication record|Meta-review" /tmp/grokrxiv-review-prod.html
! rg -n "Coming Soon|Squarespace" /tmp/grokrxiv-review-prod.html
```

Expected: Vercel headers are present, the review content is present, and Squarespace text is absent.

- [ ] **Step 3: Verify future run contract with `--no-lean`**

Run against production-ready env:

```bash
GROKRXIV_NO_CACHE=1 \
AGENTHERO_ANTIGRAVITY_BIN="$(command -v agy)" \
agh app run grokrxiv review 2606.23513 --type arxiv --no-lean
```

Expected: output contains `Lean formalization: not queued (disabled_by_flag)` and the resulting review URL renders on `https://grokrxiv.org/reviews/<review_id>`.

## References

- Vercel deployment overview: https://vercel.com/docs/deployments
- Vercel CLI deploy and prebuilt deploy: https://vercel.com/docs/cli/deploy
- Vercel monorepo setup: https://vercel.com/docs/monorepos
- Vercel custom domain setup: https://vercel.com/docs/domains/working-with-domains/add-a-domain
