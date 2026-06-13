# Golden Corpus Loop — run / check / dev / fix

Operating manual for an autonomous coding agent (Claude Code or Codex) driving
the GrokRxiv review pipeline to green against `evals/corpus.yaml`. Loop until
the exit criterion; never weaken ground truth to get there.

## Exit criterion

Two consecutive full-corpus sweeps with: every entry matching its `expected:`
block, zero NEVER-events, on **both runners** (`cli` first, then `api`).

## Hard guardrails (read before every iteration)

1. **Never edit an `expected:` block or `never_events` to make a run pass.**
   Proposed ground-truth changes go in the PR description for human sign-off.
2. **N5 halt**: Lean PROVED on Tier C/G → stop the loop, write the dossier,
   escalate. Do not "fix" anything.
3. Layering: app defects are fixed under `agenthero/apps/grokrxiv/`; touch root
   `crates/` only for app-agnostic platform gaps, and keep structural tests green.
4. Evidence rule: cite raw output, finish_reason, exit codes. Never mask a
   failure by raising token caps or timeouts without a diagnosed cause.
5. Corpus runs must never publish or open PRs: never invoke `approve`,
   `request-revisions`, or publisher actions from this loop.
6. After CLI changes: `cargo install --path crates/orchestrator --force --locked`
   and test the PATH `agh` binary, not the workspace build.
7. Ledger discipline: append one line per iteration to `evals/results/LEDGER.md`
   (timestamp, git SHA, entry id, runner, verdict, action taken). Commit fixes
   per repo conventions; one defect per commit.

## Phase 1 — RUN

Preflight once per sweep through the pinned corpus toolchain environment:
`agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh doctor`; verify
`ghc`, `lake`, `lean` through the same wrapper and record versions; record git
SHA of app contracts (app.yaml, dags/, agents/, prompts/, schemas/) into
`evals/results/<sweep-ts>/provenance.json`.

The wrapper prepends `evals/bin/` to PATH for the command it runs. This is
intentional: it makes `ghc` resolve to the GHC version pinned in
`evals/toolchain.lock.yaml` without editing operator shell startup files.

For each corpus entry (failing entries first, then full sweep):

```sh
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env \
  agh --json app run grokrxiv review <source> --loop --debug --no-external-actions \
  |& tee evals/results/<sweep-ts>/<entry-id>/run.log
```

Capture `review_id` from output. Artifacts land in
`.agenthero/artifacts/grokrxiv/reviews/<review_id>/` (review_loop/ subtree).

## Phase 2 — CHECK (mechanical; no judgment calls)

Order matters — NEVER-events first:

1. **NEVER-events N1–N5** (see corpus.yaml). Any hit → record, classify as
   pipeline defect, skip straight to DEV with priority over expected-block diffs.
2. **Artifact presence**: every output declared in `dags/review-loop.yaml`
   exists or carries an explicit `skip_reason`.
3. **Schema validity**: validate each artifact against its schema in `schemas/`
   (jsonschema CLI or python). `agh app run grokrxiv verify <review_id>` where
   applicable.
4. **Independent re-verification** (trust = re-derivability):
   - `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env ghc -fno-code SemanticModel.hs` re-run
   - `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env lake env lean GrokRxiv/Proofs.lean` re-run; `grep -E 'sorry|admit|axiom'`
   - DB: `review_agents` rows for dag_type='review-loop' — one row per node,
     verifier_status populated, no gaps
   - Tier B only: diff emitted Lean statement vs `fidelity_reference`
5. **Diff vs `expected:`** → write `evals/results/<sweep-ts>/<entry-id>/verdict.json`:
   `PASS` or `FAIL { stage, expected, actual, evidence_paths[] }`.

## Phase 3 — DEV (triage)

Classify every FAIL into exactly one bucket, in this order:

| Bucket | Meaning | Route |
|---|---|---|
| F1 contract | artifact missing / schema-invalid / gate fired late | fix pipeline (gates, verifiers, schemas) |
| F2 fidelity | schema-valid but unfaithful to the paper (IR/Lean statement drift) | fix transcriber prompt / emission; Tier B reference diff is the test |
| F3 toolchain | timeout, missing binary, runner auth, truncation | fix infra; chunk work; never blind-bump limits |
| F4 cascade | failed because upstream failed | fix the upstream defect only |
| F5 honest negative | deterministic checker rejected faithful content | NOT a defect — verify it matches `expected:` and pass |

Stage → owner map for fixes:
- extraction/body completeness → `crates/ingest`, `crates/extraction`, ingest_pipeline tools
- claims/IR/obligations/adequacy → `crates/review-loop`, `schemas/semantic_ir.schema.json`
- haskell/lean loop → `agents/review-loop/`, `prompts/review-loop/`, review-fix-code harness in app orchestrator
- citations → citation_validation tools (`dag_tools.rs` handlers), citation agents; see "Citation reliability program" below
- gates/recommendation → `review_gate.rs`, policy/meta logic in app orchestrator

Write a dossier per defect: `evals/results/<sweep-ts>/<entry-id>/dossier.md`
(symptom, bucket, root cause with file:line, fix plan, corpus entries affected).

## Phase 4 — FIX

1. Branch per repo conventions.
2. **TDD**: add a failing fixture test in app `tests/` reproducing the defect
   (e.g. an extract fixture with `sections: []` must abort the review — N1).
3. Implement minimal fix. `cargo test` (app workspace + root structural tests).
4. Re-run the affected corpus entry; then the full sweep; then the `api` runner
   sweep before merge.
5. Update LEDGER. If the same entry fails 3 consecutive fix attempts → stop,
   escalate to human with the dossier.

## Seeded backlog (from grokrxiv-reviews PR #54 — do these first)

1. **N1 extraction-completeness gate**: review aborts when `sections[]` empty /
   body density below threshold / zero theorem envs in a math-amenable paper.
   The PR-54 run reviewed abstract+bibliography and still produced a verdict.
2. **N2 explicit specialist failure**: citation specialist timeout must emit a
   failed artifact + gate fail, not vanish from the bundle.
3. **N3 gate input completeness**: policy gate / meta recommendation requires
   all upstream artifacts present + schema-valid + extraction flag green.
4. **Citation reliability program** (tracked by `regression-pr54-weyl` and
   Tier E expectations):
   - Resolver waterfall per reference, deterministic-first with caching
     (generic cache tables): Crossref → OpenAlex → Semantic Scholar →
     **NASA ADS** (gr-qc/math-ph classics) → INSPIRE-HEP → zbMATH Open (math).
     Normalize/transliterate (the 8 PR-54 misses are pre-DOI German/proceedings
     classics: Cartan, EPS, Ehlers, Kuenzle, Trautman, Reichenbach).
   - Retraction screen via Crossref retraction metadata / Retraction Watch
     (tested by `majorana-quantized`).
   - LLM adjudication only for the residue: **Gemini with search grounding**
     (gemini provider exists in llm-adapter; citation review already on
     gemini-2.5-pro) — require URL evidence in the output; on disagreement,
     second-provider quorum (claude) before `needs_review`.
   - Robustness: chunked fan-out with per-chunk timeouts and budgets; always
     emit per-reference statuses (`verified_via: crossref|ads|zbmath|gemini_grounded`,
     `unresolved`, `retracted`, `not_found`); a wholesale-empty citation
     artifact is N2.
5. **Author Tier E/F/G synthetic papers** under `evals/synthetic/` per the
   specs in corpus.yaml, then enable those entries.
