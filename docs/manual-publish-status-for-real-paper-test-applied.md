# Manual transition `reviews.status` `pr_open → published` for RPT1 (2026-05-15)

> Historical note: this was an RPT1 workaround. Current public visibility is
> `reviews.visibility = 'public'` plus `status IN ('pr_open','published',
> 'corrected','rejected')`; `pr_open` rows are visible as human-gated PR
> handoffs, and the merge webhook flips them to `published`.

## What

Direct SQL UPDATE flipped 3 review rows from `status='pr_open'` to `status='published'` (with `published_at = now()`):

```sql
UPDATE reviews
SET status = 'published',
    published_at = now()
WHERE id IN (
  'c5155ecf-6544-41ea-ad20-64ea88c7a79d',  -- 2605.00403 (math-ph)
  '72aebae7-518e-4f90-b659-a4245857496a',  -- 2605.13993 (quant-ph)
  '2d15dcff-71f5-45f0-8fe1-9bbfd858736e'   -- 2605.15132 (cs.AI)
);
```

## Why

At the time, the Supabase RLS policy `reviews_public_read` gated anon-role visibility on `status IN ('published', 'corrected')`. After `approve`, the supervisor set status to `'pr_open'` and wrote the GitHub PR URL, but the web frontend at `localhost:3000` did not show the review in this state.

The intended next-step transition `pr_open → published` is supposed to be driven by a GitHub webhook fired when the PR is merged. That webhook is FP7+ scope and not yet wired. Without it the review is invisible to the public web frontend.

For RPT1 we needed end-to-end visibility (the operator's directive was "paper review needs to be visible from web interface once done"). The smallest possible change is to manually flip status. The GitHub PR stays OPEN — DB and PR state intentionally diverge for this validation pass.

## Risk

| Risk | Mitigation |
|---|---|
| DB says published, PR is still open — looks weird to a reviewer | Acceptable for the validation pass; both states are intentional and documented |
| If we later wire the webhook, it might try to flip again or get confused | The webhook should be idempotent — a row already at `published` stays at `published`. FP7+ implementation should use `UPDATE ... WHERE status = 'pr_open'` semantics |
| Operator may want to keep `pr_open` semantics longer | One-line revert (below) restores the DB state |

## Reversal

```sql
UPDATE reviews
SET status = 'pr_open',
    published_at = NULL
WHERE id IN (
  'c5155ecf-6544-41ea-ad20-64ea88c7a79d',
  '72aebae7-518e-4f90-b659-a4245857496a',
  '2d15dcff-71f5-45f0-8fe1-9bbfd858736e'
);
```

## Verification

After the UPDATE + a Next.js dev server restart (Cache Components reload):

```sh
curl -s "http://localhost:3000/api/v1/papers/2605.00403" | jq -r '.reviews[0].status'
# → "published"

curl -s "http://localhost:3000/reviews/c5155ecf-6544-41ea-ad20-64ea88c7a79d" | grep -oE '<title>[^<]+'
# → "Generalized Fourier Transforms..." (was "Review not found")
```

All three review pages, all three paper pages, and the homepage grid now show the new content.

## Follow-up

FP7 should implement the actual GitHub webhook → `published` transition, replacing this manual step. Once it lands, this doc becomes a historical reference for "the one time we did it by hand."
