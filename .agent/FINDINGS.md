# GrokRxiv Local Harness Findings

## P0-001: Product Review Loop Cannot Start From PATH Runtime

ID: P0-001
Corpus entry: `regression-pr54-weyl`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 1
finish_reason: adapter/runtime argument parse failure
Bucket: F3 toolchain
NEVER-event: none reached; review did not start
Symptom: PATH `agh` reaches the GrokRxiv app adapter, but the installed `/Users/mlong/.cargo/bin/grokrxiv-app` rejects `--loop` before any corpus artifact is produced.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-url.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/runtime-installed-dry-run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/runtime-source-url-dry-run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/provenance.json`
Artifact paths: none; review did not start.
Root cause: installed runtime binary is stale. The manifest declares `--loop` and current source parses it, but PATH `grokrxiv-app` predates that parser.
Owning code/surface:
- `/Users/mlong/.cargo/bin/grokrxiv-app`
- `agenthero/apps/grokrxiv/rust/src/main.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
- `agenthero/apps/grokrxiv/app.yaml`
Fix plan:
1. Install current app runtime binary: `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`.
2. Install current app adapter binary: `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`.
3. Re-run `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`.
4. If it still fails before review start, add product-surface coverage that executes the adapter/runtime path, then fix adapter runtime resolution.
Attempts: 1
Escalation status: none.

## P0-001 Resolution

Status: fixed locally, 2026-06-12T23:27Z.
Evidence:
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass.
- `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`: pass.
- `agh --dry-run app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`: pass, emitted review-loop stage plan.
- Real product run then started and completed as review `eca527eb-3930-49e6-a828-66dd64611430`.

## P0-002: Corpus Loop Opened A PR Despite No-Publishing Guardrail

ID: P0-002
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: none declared yet; violates `LOOP.md` hard guardrail 5.
Symptom: the corpus command opened `https://github.com/GrokRxiv/grokrxiv-reviews/pull/55` with `pr_kind=revision_needed` and `status=pr_open`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-after-install.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-002-no-pr-guardrail.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/publish_decision.json`
Root cause: no safe corpus/eval no-external-side-effects mode is wired into the product review-loop command.
Fix plan: add product-surface coverage and a safe local corpus command that disables approve, request-revisions, publisher, and revision PR creation before any further full corpus reruns.
Attempts: 1
Escalation status: PR #55 was opened by the run; do not invoke close/withdraw from the corpus loop without human direction.

## P0-003: N1 Extraction Completeness Gate Did Not Fire

ID: P0-003
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: N1_review_on_empty_body
Symptom: review proceeded despite empty extraction artifacts: `body.md` is 0 bytes, `sections.json` has 0 sections, `equations.json` has 0 equations, and `theorem_graph.json` has 0 nodes. The extraction report marked these stages `ok`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-003-n1-extraction-gate.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/sections.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/extraction_report.json`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/paper_math_sources.json`
Root cause: ingest/extraction can mark empty body/theorem artifacts successful, and review/policy does not require extraction completeness before downstream verdicts and PR actions.
Fix plan: write failing fixture test for empty body/sections/theorem graph, add extraction-completeness failure artifact, and abort before specialist/meta/policy/PR stages.
Attempts: 1
Escalation status: none.

## P0-004: Citation Waterfall Not Wired For PR-54 Classics

ID: P0-004
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: none.
Symptom: citation validation checked 53 references and emitted partial evidence, but left 8 unverified; all evidence came from `crossref_bibliographic`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-004-citation-waterfall.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/citation_validation_report.json`
Root cause: resolver waterfall is not implemented or not wired into this review-loop path; ADS, zbMATH, OpenAlex, INSPIRE, and Gemini-grounded adjudication evidence are absent.
Fix plan: add Weyl-classics citation fixture, implement deterministic waterfall/cache, preserve per-reference partial statuses, and require unverified/needs_review count `<= 2`.
Attempts: 1
Escalation status: none.

## P0-005: PR Fixer Timed Out After 360 Seconds

ID: P0-005
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F3 toolchain
NEVER-event: none.
Symptom: `pr_artifact_fixer` timed out after 360 seconds; `pr_fixes.json` reports fixed `review.pdf` was not produced.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-005-pr-fixer-timeout.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/pr_fixes.json`
Root cause: unknown from this audit; likely downstream of P0-003 because the loop should not enter PR fixing after invalid extraction.
Fix plan: do not tune timeouts yet; fix P0-002 and P0-003 first, then rerun if PR fixing is still reachable on valid extraction.
Attempts: 1
Escalation status: deferred.

## Finding Template

Use one dossier per defect.

```text
ID:
Corpus entry:
Runner:
Command:
Exit code:
finish_reason:
Bucket: F1 contract | F2 fidelity | F3 toolchain | F4 cascade | F5 honest_negative
NEVER-event:
Symptom:
Raw evidence paths:
Artifact paths:
Root cause:
Owning code:
Fix plan:
Attempts:
Escalation status:
```
