# GrokRxiv Local Harness Next Steps

Continue exactly from here.

## Current Coordinator State

- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- Latest coordinator checkpoint before this state update: `beddef4` (`codex checkpoint: P0 - P0-043 coordinator merge`)
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
- Corpus runs must keep `--no-external-actions`; never invoke approve, request-revisions, publisher, close, withdraw, merge, or PR-opening actions.

## Immediate Queue

### 1. P0-044 Acceptance / Merge

Worker:

```text
.agent/worktrees/p0-044-zeta-haskell-target-hygiene
```

Status:

- Worker branch is clean at `2273503` (`codex checkpoint: P0 - P0-044 Haskell target hygiene`).
- It prevents bibliography/reference math snippets and partial semantic gaps from becoming required proof obligations.
- Worker tests passed before commit, but the affected zeta rerun stalled before Haskell artifacts. Treat the rerun as inconclusive F3, not pass/fail.

Next action:

1. In the worker, run a bounded affected rerun for `zeta3-irrationality` with `--no-external-actions`.
2. If it stalls again, write an F3 stall dossier and move to P0-046 before merge.
3. If it completes, verify Haskell/Lean only receive real theorem targets, then coordinator-merge and rerun focused tests.

### 2. P0-045 No-Math Proof Skip

Add fixture coverage for a non-math document:

- normalize/extract succeeds;
- semantic math map reports no formal targets;
- Haskell artifact exists as an explicit skip with `skip_reason: no_math_targets`;
- Lean artifact exists as an explicit skip with `skip_reason: no_math_targets`;
- review/PR artifact still builds under `--no-external-actions`;
- git/web report shows proof stages as `NOT_CONDUCIVE_TO_LEAN_PROOF` or the schema-compatible skip equivalent.

### 3. P0-046 Harness Timeout Detection

Add bounded run/stall detection so a stuck corpus run self-classifies as F3 with:

- command;
- PID/process state when killed;
- elapsed time;
- last log line or artifact timestamp;
- exit code or signal;
- raw log path.

Do this before the next full sweep.

### 4. P0-039 Withdrawn Bertrand Source

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

Start with P0-044 acceptance. If a run stalls, classify it as F3 and move to
P0-046 harness timeout detection. Do not weaken corpus expected blocks or
NEVER-events. Do not run external publishing actions.
```
