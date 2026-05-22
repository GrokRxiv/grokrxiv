# arXiv fetch — retry with exponential backoff (2026-05-15)

## What

`crates/ingest/src/download.rs::rate_limited_get` now retries on transient failures (HTTP 5xx, HTTP 429, transport-level errors) with backoff schedule `[0s, 2s, 5s, 12s, 30s]`. Hard 4xx errors (other than 429) are surfaced immediately without retry.

## Why

During the RPT1 3-paper parallel-ingest test, 2 of 3 cargo processes failed at `fetch arxiv metadata`. The intra-process semaphore in `download.rs` serialises requests within ONE process; when multiple `cargo run -- ingest` invocations run in parallel, each has its OWN semaphore and they all hit arXiv simultaneously, triggering arXiv's load shedding.

Symptoms before this fix:

```
{"level":"ERROR","fields":{"message":"command failed","err":"ingest: fetch arxiv metadata for 2605.00403"}}
{"level":"ERROR","fields":{"message":"command failed","err":"ingest: fetch arxiv metadata for 2605.13993"}}
arxiv_id=2605.15132 review_id=7cdf0db9-... (only the third succeeded)
```

With backoff, the second and third parallel requests should sleep briefly and retry, succeeding on the second or third attempt.

## How

| Change | Location |
|---|---|
| Retry loop with backoff schedule | `crates/ingest/src/download.rs::rate_limited_get` |
| Transient classification: 5xx or 429 → retry; other 4xx → break immediately | `download.rs` inside the loop |
| Transport-level errors (DNS, connect timeout, reset) → all treated as transient | `download.rs` |
| Log on each retry (`warn!` with url + attempt) | tracing-friendly |

No API surface change — `rate_limited_get` signature is identical.

## Risk

| Risk | Mitigation |
|---|---|
| Total ingest wall time grows from ~3s to up to ~50s on hard failure | Acceptable — ingest is a once-per-paper cost. The DAG dominates wall time anyway (~90s). |
| Persistent arXiv outage masks as slow ingest | Worst-case 50s + 4 retries before hard failure; logs make this obvious at INFO level. |
| Multiple processes still uncoordinated | Documented as a known limitation. A future enhancement could use a file lock (`flock` on a sentinel file) to coordinate across processes; not needed at 3-paper scale. |

## Reversal

`git diff crates/ingest/src/download.rs` → revert the rate_limited_get change. The retry path was added in one contiguous block.

## Verification

```sh
cargo build -p grokrxiv-ingest     # clean
cargo test -p grokrxiv-ingest      # tests green (no new tests; retry path is timing-sensitive and tested manually by re-running the parallel ingest)
```

End-to-end: re-run the parallel ingest of 2605.00403 + 2605.13993 + 2605.15132. Expect all three to succeed; expect to see one or two `arxiv transient failure; backing off` warnings in the logs as the retry path kicks in.
