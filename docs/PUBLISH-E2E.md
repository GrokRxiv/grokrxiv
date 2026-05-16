# Publish E2E — proving the moderation → PR → merge → publish loop end-to-end

`scripts/publish-e2e.sh` is **opt-in**. It exercises the full publish path against
a real GitHub repository. `just smoke` deliberately skips it because it needs
GitHub credentials that aren't required for any other test. Use this doc when
you want to prove the publication loop actually closes.

## What it proves

1. `grokrxiv --runner cli --extractor cli --no-cache --json ingest <arxiv_id>`
   produces a real review row at `awaiting_moderation` with six
   `review_agents` rows, real input/output JSON artifacts, and per-role
   verifier evidence persisted. Set `RUNNER=api EXTRACTOR=api` only when you
   intend to spend provider API credits.
2. `grokrxiv approve <review_id>` opens a real pull request against your test
   reviews repo with the rendered HTML/MD/LaTeX/zip artifacts at the canonical
   `reviews/YYYY/MM/<field>/<arxiv_id>/` repo path.
3. `gh pr merge --merge --delete-branch` merges the PR.
4. A synthetic GitHub `pull_request.closed` webhook (HMAC-signed against the
   orchestrator's `GITHUB_WEBHOOK_SECRET`) posted to `localhost:8080/webhook/github`
   transitions the review to `status=published`.
5. `GET /api/v1/reviews/<review_id>` returns the review with `status:"published"`,
   confirming RLS + the public read path.

## One-time setup

```bash
# 1. Use the configured reviews repository, or create a disposable test repo.
gh repo create GrokRxiv/grokrxiv-reviews --public --description "GrokRxiv reviews"

# 2. Create a fine-grained PAT scoped to that repo with these permissions:
#      - Contents: Read and write
#      - Pull requests: Read and write
#    Save it as GITHUB_TOKEN.
#    Generate at: https://github.com/settings/personal-access-tokens/new

# 3. Pick (or reuse) a webhook HMAC secret. It just needs to match what the
#    orchestrator already runs with — by default that's GITHUB_WEBHOOK_SECRET
#    from your `.env`.
```

## Run it

From the repo root with the local stack already up (`supabase start && docker compose up -d`):

```bash
export GITHUB_TOKEN="ghp_..."
export GROKRXIV_REVIEWS_OWNER="GrokRxiv"
export GROKRXIV_REVIEWS_REPO="grokrxiv-reviews"
export GITHUB_WEBHOOK_SECRET="$(grep ^GITHUB_WEBHOOK_SECRET= .env | cut -d= -f2)"
export DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:54322/postgres"
export RUNNER=cli
export EXTRACTOR=cli

bash scripts/publish-e2e.sh
```

The script prints each step (1. ingest, 2. approve, 3. merge, 4. webhook,
5. status check, 6. public API). On success, the last line is:

```
✓ publish E2E PASSED end-to-end.
```

Any failed step aborts with a single-line error and a non-zero exit code.

## Override the test paper

```bash
ARXIV_ID=2605.12492 bash scripts/publish-e2e.sh
```

## Cleaning up between runs

Each successful run merges + closes a PR. To re-run cleanly:

```bash
# Reset the reviews repo's main branch:
gh api -X PATCH "repos/${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}/git/refs/heads/main" -f sha=$(gh api repos/${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}/git/refs/heads/main -q .object.sha)

# (Or just empty the contents on your test branch — the publisher always
# branches off `main` and writes a fresh tree.)
```

Local DB cleanup, if you want to start from a clean Supabase row set:

```bash
docker exec -i $(docker ps -qf name=supabase_db_grokrxiv) psql -U postgres -d postgres -c "
  delete from review_agents;
  delete from corrections;
  delete from moderation_queue;
  delete from reviews;
  delete from papers where arxiv_id <> '2401.12345';
"
```

(Keeps the seed paper `2401.12345` for the Playwright JSON-LD spec.)

## What this script does NOT cover

- Vercel revalidation in production. The script POSTs to the orchestrator's
  `/webhook/github`, which fires a `revalidatePath` against the local Next.js
  app. In production, Vercel's ISR cache invalidation goes through Vercel's
  own webhook plumbing, which is exercised by the existing
  `.github/workflows/reviews-merge.yml.example` workflow (deployed in the
  `reviews` repo, not this one).
- Email / X drafts to the paper authors. That's an M4 surface; the publish
  script doesn't try to assert on it.
- Multi-reviewer concurrency. Each run touches a single review.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `approve` simulates the PR instead of opening one | `GITHUB_TOKEN` not in env | `export GITHUB_TOKEN=...` |
| Webhook returns 401 | `GITHUB_WEBHOOK_SECRET` mismatch between script env and orchestrator container env | restart orchestrator with matching secret, or update the script's env |
| `gh pr merge` errors with "Merging is blocked" | Branch protection on the test repo | disable protection on the test repo's `main`, or use `--squash` |
| `/api/v1/reviews/<id>` returns 404 after step 5 | Supabase RLS hasn't picked up the row | wait a second + retry; if persistent, check the `reviews_public_read` policy at `migrations/20250513000002_rls.sql` |
