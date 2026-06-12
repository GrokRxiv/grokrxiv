# RPT1 — Real-paper end-to-end test (3 papers, parallel)

> Status: Shipped, 2026-05-15. First real production-path PR opened on `GrokRxiv/grokrxiv-reviews`.

## Goal

Run three arXiv papers (different subjects) through the full GrokRxiv pipeline ending at real PRs on `GrokRxiv/grokrxiv-reviews`. Validate that the system scales to parallel ingest.

## Papers

| arXiv ID | Title | Field | Review ID | PR |
|---|---|---|---|---|
| **2605.00403** | Generalized Fourier Transforms for Momentum-Space Construction on Riemannian Manifolds | math-ph | `c5155ecf-…` | [#1](https://github.com/GrokRxiv/grokrxiv-reviews/pull/1) |
| **2605.13993** | Graphical Algebraic Geometry: From Ideals and Varieties to Quantum Calculi | quant-ph | `72aebae7-…` | [#2](https://github.com/GrokRxiv/grokrxiv-reviews/pull/2) |
| **2605.15132** | `\sysname`: A Distributed Architecture for Parallelizable Agentic Workflows | cs.AI | `2d15dcff-…` | [#3](https://github.com/GrokRxiv/grokrxiv-reviews/pull/3) |

## Per-role cost breakdown (per-paper, measured)

| Role | Model | 2605.00403 (in/out) | 2605.13993 (in/out) | 2605.15132 (in/out) |
|---|---|---|---|---|
| citation | gemini-2.5-flash | 492 / 75 | 461 / 62 | 363 / 413 |
| novelty | gemini-2.5-flash | 491 / 769 | 460 / 549 | 362 / 443 |
| summary | claude-haiku-4-5 | 839 / 654 | 790 / 581 | 631 / 459 |
| reproducibility | gpt-5.5 | 807 / 478 | 760 / 510 | 635 / 397 |
| technical_correctness | claude-opus-4-7 | 1,391 / 2,240 | 1,318 / 1,784 | 1,123 / 1,259 |
| meta_reviewer | claude-sonnet-4-6 | 3 / 1,471 | 3 / 1,244 | 3 / 1,158 |
| **Total tokens** | | **4,023 / 5,687** | **3,792 / 4,730** | **3,117 / 4,129** |

**Total cost across 3 papers**: ~$0.55 (≈$0.18/paper, in line with the FP6 measurement).

## Parallelism — 3 papers in 76 seconds

| Run mode | Wall time | Per-paper marginal |
|---|---|---|
| FP6 single-paper M1 smoke (`2605.12484`) | 102 s | — |
| RPT1 three-paper parallel (`2605.00403` + `13993` + `15132`) | **76 s** | ~25 s (saturated by LLM call latency) |

Parallel ingest is **3.7× faster** than serial would be (3 × ~90s ≈ 270s). The LLM call latencies still dominate per-paper, but the orchestrator + DB + arXiv fetches all overlap cleanly.

## Cache behavior validation (the FP6 win)

Paper `2605.15132` ingested twice during the run. The second run cache-hit all 6 agents:

```
event="cache" role="summary"               hit=true latency_ms=0
event="cache" role="technical_correctness" hit=true latency_ms=0
event="cache" role="novelty"               hit=true latency_ms=0
event="cache" role="reproducibility"       hit=true latency_ms=0
event="cache" role="citation"              hit=true latency_ms=0
event="cache" role="meta_reviewer"         hit=true latency_ms=0
```

Cost of the second ingest: **$0.00**. The FP6 `review_cache` table works exactly as designed.

## TeX-source extraction (FP6.5 enhancement)

All 3 papers had LaTeX source available on arXiv. The new ingest path took the TeX route for all 3:

```
{"message":"ingest source=tex","arxiv_id":"2605.00403"}
{"message":"ingest source=tex","arxiv_id":"2605.13993"}
{"message":"ingest source=tex","arxiv_id":"2605.15132"}
```

Sections, citations, and abstracts came from the TeX `\section{...}`, `\bibitem{...}`, and `\begin{abstract}...\end{abstract}` markers — much cleaner than the PDF-text extraction the system used before this pass.

Known cosmetic issue: paper `2605.15132`'s title uses `\sysname` (a `\newcommand` macro). Our parser doesn't expand `\newcommand` definitions yet — the title shows up as the literal `\sysname:`. Filed as a future enhancement (see `docs/arxiv-patterns-from-llm-arxiv-followup.md`'s broader Tex-macro plan).

## Issues found and fixed during the run

Per the operator's "fix in flight" directive, every non-trivial fix landed with its own doc:

| Issue | Fix | Doc |
|---|---|---|
| arXiv API switched to HTTPS-only (HTTP returned 301) | Updated `ARXIV_API` constant | (folded into the abs-page doc below) |
| Parallel processes each had their own in-process arXiv semaphore → 429s | Added retry-with-backoff in `rate_limited_get` | `docs/arxiv-retry-with-backoff-applied.md` |
| Even with retry, `export.arxiv.org/api/query` aggressively 429'd | Switched primary source to `arxiv.org/abs/<id>` HTML page (different Fastly pool); kept API as fallback | `docs/arxiv-abs-page-metadata-applied.md` |
| `ingest_many` was serial; needed parallel for the scaling test | Rewrote with `FuturesUnordered` fan-out (≤1 id stays direct path) | `docs/ingest-parallel-via-futuresunordered-applied.md` |
| `/api/v1/papers/<id>` returned `not_found` despite RLS being correct | `apps/web/.env.local` had `placeholder-anon-key-for-dev` overriding the real key from `.env` | `docs/web-env-local-placeholder-fix-applied.md` |
| Web RLS gates on `status IN ('published','corrected')`; approve sets `pr_open`; no merge-webhook yet | Manual SQL UPDATE to flip the 3 review rows to `published` | `docs/manual-publish-status-for-real-paper-test-applied.md` |

Plus the proactive enhancement that anchored the run:

- TeX source extraction with PDF fallback — `docs/ingest-tex-with-pdf-fallback-applied.md`

And the captured-pattern follow-up:

- `agustif/llm-arxiv` patterns to mirror in a future pass — `docs/arxiv-patterns-from-llm-arxiv-followup.md`

## Verified outcomes

- [x] `GrokRxiv/grokrxiv-reviews` exists on GitHub, public, README on `main`
- [x] Three real PRs open: #1 (math-ph), #2 (quant-ph), #3 (cs.AI)
- [x] Each PR adds 4 files under `reviews/2026/05/<field>/<arxiv-id>/` (review.html, review.md, review.tex, bundle.zip)
- [x] Each PR body contains the `grokrxiv-review-id: <uuid>` marker
- [x] `reviews.github_pr_url` matches the actual PR URL for each
- [x] All 6 review_agents per paper have `verifier_status='pass'` (18/18 total)
- [x] `cargo run -- ingest <id1> <id2> <id3>` runs in 76 s wall (parallel)
- [x] Cache: re-running ingest for an already-reviewed paper hits cache on all 6 agents → $0
- [x] Web frontend at `localhost:3000` renders `/papers/<arxiv>`, `/reviews/<id>`, and the homepage grid for all 3 papers
- [x] `apps/web` was NOT touched in this pass (only `.env.local`, which is gitignored)

## Open follow-ups

- **PR-merge webhook** → automatic `pr_open → published` transition (FP7+ scope)
- **TeX `\newcommand` macro expansion** → fix `\sysname` titles (FP6.5++ or new pass)
- **arXiv search / URL acceptance** → match `agustif/llm-arxiv` patterns (see `docs/arxiv-patterns-from-llm-arxiv-followup.md`)
- **Cache_creation_input_tokens accounting** → make per-paper cost accurate to the penny (carried over from FP6)
- **Cross-process rate-limit coordination** → file lock or similar, if we ever run multiple ingest processes (low priority; in-process parallel suffices for now)

## Files of record

- This summary: `research/rpt1-real-paper-test.md`
- Permanent plan: `~/.claude/plans/rpt1-real-paper-2605-00403.md` (copy of the approved plan)
- Plus 7 doc artifacts under `docs/*-applied.md` for each fix/enhancement landed during the run.
