# Parallel ingest via FuturesUnordered (2026-05-15)

## What

`cargo run -- ingest <id1> <id2> <id3> ...` now ingests papers **concurrently** within a single process instead of serially. Single-paper invocations stay on the direct path (so M1 smoke output shape is unchanged).

## Why

RPT1 needed to validate that the pipeline scales to N>1 papers without code changes. Two design constraints:

1. The arXiv rate-limit semaphore in `crates/ingest/src/download.rs` is in-process — multiple parallel `cargo run` invocations have separate semaphores and all hit arXiv simultaneously, triggering 429s. Single-process fan-out keeps the semaphore meaningful.
2. The DAG inside `run_one_paper_blocking` is mostly I/O-bound (LLM API waits, DB writes) — fanning out gives near-perfect speedup with no GPU/CPU contention.

Measured outcome on three 2026-05 papers (math-ph, quant-ph, cs.AI): **76s wall** for 3 papers vs ~90s for a single paper before. Effectively zero parallel overhead.

## How

| Change | Location |
|---|---|
| `ingest_many` rewritten: ≤1 id → direct path, >1 ids → FuturesUnordered fan-out | `crates/orchestrator/src/cli.rs` |
| Each task captures `Supervisor::clone()` + `AppState::clone()`; both already implement Clone for sharing across tokio tasks | `cli.rs` |
| Error reporting per-paper with a final `bail!` if ≥1 failed | `cli.rs` |

`futures` crate was already a transitive dep. No new deps.

## Risk

| Risk | Mitigation |
|---|---|
| Concurrent DB writes on `review_inputs` (the FP6 dedup table) — unique PK on `review_id` makes this safe; each ingest gets a fresh UUID | Verified empirically — 3 concurrent ingests, no PK collision |
| Concurrent writes to `papers` (upsert on `arxiv_id`) — Postgres serializes via row lock; one wins, others see it as existing | Tested with paper 2605.15132 (re-ingested twice — second run cache-hit all 6 agents, zero LLM cost) |
| LLM API rate limits across providers — Anthropic/OpenAI/Gemini all accept concurrent requests up to per-key TPM ceilings; we're well below those at 3-paper scale | Verified — 18 concurrent LLM calls completed cleanly |
| Memory growth — each parallel ingest holds a `PaperExtract` (~180KB for math-ph paper) + LLM response buffers | Negligible at N=3; consider chunking via `buffer_unordered(N)` if scaling to 30+ |

## Reversal

```sh
git checkout HEAD~1 -- crates/orchestrator/src/cli.rs
```

Reverts to the serial for-loop. Existing M1 smoke (single id) is unaffected by this change in either direction.

## Verification

```sh
cargo run --quiet -- ingest 2605.00403 2605.13993 2605.15132
# Look for three near-simultaneous "M1: ingest start" log lines, then
# three "review_id=..." outputs at the end.
```
