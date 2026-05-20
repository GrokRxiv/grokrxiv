# Webhook and scheduler hardening

## Webhook delivery handling

`POST /webhook/github` now treats the GitHub delivery id as the idempotency
key before doing expensive work:

- verified `pull_request.closed` merge events are recorded in `review_events`
  and acknowledged before the publish finalizer runs in a background task.
- verified `pull_request.synchronize` events are recorded in `review_events`
  and acknowledged before the re-review lookup/enqueue work runs in a
  background task.
- duplicate `X-GitHub-Delivery` values are acknowledged without repeating the
  finalizer or re-review side effects.

Review correlation now checks PR title markers and labels before falling back
to the legacy PR body marker. Accepted marker format is
`grokrxiv-review-id:<uuid>` or `grokrxiv-review-id: <uuid>`.

## Scheduler backfill handling

Startup backfill no longer runs inline before the daily scheduler loop. The
scheduler starts its daily loop immediately and launches backfill in a sibling
task.

Backfill is chunked into seven-day ranges. A chunk only advances after the
listing fetch/enqueue path succeeds, so a transient arXiv outage does not skip
the rest of the backfill while the process stays alive. Listing fetches retry
with backoff before the same chunk is retried.

Daily listing failures also retain the failed start date for the next tick, so
the next successful daily run covers the missed range instead of dropping it.

## Remaining gap

This pass did not add a persistent scheduler checkpoint table. The retry cursor
is process-local; a durable database checkpoint can be added in a later schema
pass without changing the webhook delivery-event schema.
