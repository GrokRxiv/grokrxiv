# GrokRxiv Local Harness Next Steps

Continue exactly from here.

## Current Coordinator State

- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- Latest merged worker checkpoint: `d373291` (`codex checkpoint: P0 - harness timeout detection`)
- Pending worker checkpoint: none.
- Current phase: P0 stabilize, narrowed to the vertical review-pipeline slice.
- Baseline tag: none.
- Last green full sweep: none.
- Run model: local Codex only; do not use Codex Cloud.

## Narrow Acceptance Contract

The near-term goal is:

```text
file/source -> normalized content -> semantic math map -> conditional Haskell/Lean proof path -> LLM review/PR artifact -> git/web evidence report
```

Rules:

- Source and extraction must be reliable. Missing body content fails before any verdict.
- Normalized content must preserve body text, sections, references, math/context artifacts, and provenance.
- Haskell/Lean are conditional proof stages, not universal document stages.
- If normalized content has no formal math targets, Haskell and Lean must be explicit skips with `skip_reason: no_math_targets`; the review/PR artifact path still runs.
- Use `NOT_CONDUCIVE_TO_LEAN_PROOF` as the operator-facing label for the no-math proof skip. Until schemas expose that exact enum, encode it as visible skip artifacts.
- If formal math targets exist, Haskell/Lean must run and emit `PROVED`, `NOT_PROVED`, unsafe proof status, or a classified F1-F5 failure.
- Corpus green means `integrity_ready=true`, not automatic publication. A report can be green while saying not proved, not applicable, reject, or needs review.
- `reference_ready=true` is the public-use bar: the report is good enough for another reader to use as a reference. Claims and limitations must be traceable, unresolved items explicit, and the review useful, readable, and not overclaimed.
- `publisher_ready=true` is stricter: reference-ready plus publication gate passed, recommendation policy allows publication, PR/web artifacts build, and no blockers remain. Real approval/publish actions stay outside the corpus loop.
- LLM agents should not guess what to do with missing data. Every agent call needs an input manifest with required artifacts, optional artifacts, completeness flags, provenance, and explicit missing-data instructions. Missing required data without an allowed skip fails before the LLM call.
- Corpus runs must keep `--no-external-actions`; never invoke approve, request-revisions, publisher, close, withdraw, merge, or PR-opening actions.

## Immediate Queue

### 1. P0-044 Acceptance / Merge

Status: accepted and merged to coordinator.

Evidence:

- Result root: `agenthero/apps/grokrxiv/evals/results/20260614T003026Z/zeta3-after-p0-044-acceptance`.
- Review id: `1154e7d0-ea88-48b1-90d5-fd60d5471e59`.
- Product exit: 0; external actions disabled; `pr_url=null`.
- `semantic_category_mapper`: `theorem_candidates=0`, `definitions=0`, `assumptions=0`.
- Haskell: `haskell_review_fix_code [OK]`, `attempts=1`, empty targets/claims/proof obligations in `SemanticModel.hs`.
- Guard strings absent from semantic/Haskell artifacts: `body_math_41`, `body_math_67`, `ReviewCategory`.
- PR artifact path completed; citation validation was non-blocking.

Coordinator verification:

- `grokrxiv-review-loop` tests passed 16/16.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- structural tests passed 45/45.
- `git diff --check` passed.

Next action: already completed by P0-045.

### 2. P0-045 No-Math Proof Skip

Status: accepted and merged to coordinator.

Evidence:

- Result root: `agenthero/apps/grokrxiv/evals/results/20260614T004910Z/zeta3-after-p0-045-no-math-skip`.
- Review id: `849e55d1-b1b8-4c5d-9b53-db9e1aa95007`.
- Product exit: 0; external actions disabled; `pr_url=null`.
- `semantic_category_mapper`: `theorem_candidates=0`, `definitions=0`, `assumptions=0`.
- `proof_obligations.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `obligations=0`.
- `lean/results.json`: `status=skipped`, `skip_reason=no_math_targets`, `verdict=NOT_PROVED`, `proof_status=SKIPPED`, `entries=0`.
- `semantic_adequacy.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `verdicts=0`.
- `policy_gate.json`: `deterministic_status=pass`, `integrity_ready=true`, `publisher_ready=false`, `blocking_issues=[]`, `publishability_vector.formal=not_conducive_to_lean_proof`.
- Review/PR artifacts built. Live stderr had a display-only `[FAIL] deterministic_status=pass`; source now uses `deterministic_status` for the marker.

Worker verification:

- `grokrxiv-review-loop` tests passed 17/17.
- focused app-runtime no-math skip test passed 1/1.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- `git diff --check` passed.

Coordinator verification:

- `git merge --ff-only p0-045-no-math-proof-skip`: pass, fast-forward to `eaaf4d4`.
- `grokrxiv-review-loop` tests passed 17/17.
- focused app-runtime no-math skip test passed 1/1.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- structural tests passed 45/45.
- `git diff --check` passed.
- PATH installs passed for `grokrxiv-app`, `agenthero-dag-app-grokrxiv`, and `agh`.
- Wrapped PATH dry-run passed with `external_actions.enabled=false`.

Next action: already completed by P0-045b; start P0-046 next.

### 2b. P0-045b LLM Input Contract Gate

Status: accepted and merged to coordinator.

Evidence:

- Red-first fixture `review_loop_agent_input_contract_rejects_missing_semantic_ir_before_agent` failed before implementation with missing helper, then passed.
- Missing Haskell semantic IR now blocks before deterministic Haskell generation or LLM runner invocation with `stage=haskell_review_fix_code`, `missing_artifact=review_loop/semantic_ir.json`, and remediation `rerun semantic_category_mapper`.
- Review-loop code-agent payloads include `input_contract` with `missing_required_input_policy=fail_before_llm_call`.
- Worker verification passed: app-runtime `review_loop` 20/20, app workspace check, structural tests 45/45, `git diff --check`, full app-runtime lib serial 295/295, PATH installs, and wrapped dry-run.
- Coordinator verification passed after fast-forward merge to `6700d28`: app-runtime `review_loop` 20/20, app workspace check, structural tests 45/45, full app-runtime lib serial 295/295, `git diff --check`, PATH installs, `agh --version`, and wrapped dry-run with `external_actions.enabled=false`.

Next action: start P0-046.

### 3. P0-046 Harness Timeout Detection

Status: accepted and merged to coordinator.

Evidence:

- Added `agenthero/apps/grokrxiv/evals/bin/grokrxiv-run-with-timeout`.
- `LOOP.md` now wraps corpus entry commands with the bounded helper and writes `run-status.json` next to `run.log`.
- Wall timeout emits `bucket=F3`, `classification=timeout`, `reason=wall_timeout`, exit 124.
- Idle-log stall emits `bucket=F3`, `classification=stall`, `reason=idle_timeout`, exit 124.
- Status JSON records command, PID, process state, elapsed time, exit code or signal, raw log path, last log line, and log mtime.
- Worker verification passed: focused corpus tests 11/11; app-runtime `review_loop` 20/20; app workspace check; structural tests 45/45; `git diff --check`; successful-wrapper smoke.
- Coordinator verification passed after fast-forward merge to `d373291`: focused corpus tests 11/11; app-runtime `review_loop` 20/20; app workspace check; structural tests 45/45; `git diff --check`; successful-wrapper smoke.

Next action: start the first bounded full local CLI corpus sweep.

### 4. First Bounded Full Local CLI Corpus Sweep

Run LOOP.md preflight and corpus entries through `grokrxiv-run-with-timeout`.
Use the generated `run-status.json` for F3 classification instead of manual
stall diagnosis. Keep `--no-external-actions`.

Triage rules:

- `bertrand-elementary` is expected to skip before review as withdrawn/unavailable v5.
- `zeta3-irrationality` should no longer be blocked by P0-044/P0-045/P0-046; if citation timeout reappears, triage it with wrapper evidence.
- No full P0 green claim until all entries pass, zero NEVER-events, structural tests stay green, and the sweep is repeated on both runners.

### 5. P0-039 Withdrawn Bertrand Source

Resolved by human sign-off on 2026-06-14:

- Keep `bertrand-elementary` pinned to withdrawn/unavailable `2407.07620v5`.
- Do not review it.
- Treat the expected outcome as a source/extraction skip:
  `source_status: withdrawn_unavailable`,
  `extraction: skipped_withdrawn_source`,
  `review_loop: skipped_before_review`,
  `skip_reason: withdrawn_or_unavailable_source`.

A retrievable `v4` replacement can be added later as a separate corpus decision.

## Resume Prompt

```text
Read .agent/AGENT_STATUS.md, .agent/FINDINGS.md, .agent/PATCH_PLAN.md,
.agent/TEST_LOG.md, .agent/NEXT_STEPS.md,
agenthero/apps/grokrxiv/evals/PHASES.md,
agenthero/apps/grokrxiv/evals/LOOP.md, and
agenthero/apps/grokrxiv/evals/results/LEDGER.md.

Continue the local-only P0 vertical slice:
file/source -> normalized content -> semantic math map -> conditional
Haskell/Lean proof path -> LLM review/PR artifact -> git/web evidence report.

Run the first bounded full local CLI corpus sweep through
`agenthero/apps/grokrxiv/evals/bin/grokrxiv-run-with-timeout`. Use
`run-status.json` for timeout/stall F3 classification. Do not weaken corpus
expected blocks or NEVER-events. Do not run external publishing actions.
```
