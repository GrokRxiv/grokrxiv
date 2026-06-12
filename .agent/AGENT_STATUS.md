# GrokRxiv Local Harness Status

Updated: 2026-06-12T23:49:59Z

## Current State

- Goal: Multi-day phased local Codex build of the GrokRxiv review pipeline on AgentHero, gated by the golden corpus.
- Current phase: P0 stabilize.
- Session type: P0 sessions 1-2 audit plus first toolchain fix.
- Branch/worktree: `grokrxiv-local-corpus-harness` in `/Users/mlong/Documents/Development/grokrxiv`.
- Branch base commit: `0f157da`.
- Baseline tag: none yet.
- Last green sweep: none yet.
- Current runner: local `cli` first; local `api` runner command must be locked during P0 audit before any two-runner green claim.
- In-flight defect: P0-002 no-publishing guardrail. P0-001 is fixed locally; the real `regression-pr54-weyl` run reached review-loop but opened PR #55 despite corpus-loop guardrails.
- Run model: local Codex only. Do not use Codex Cloud tasks, cloud apply, or cloud state.
- Agent-team model: coordinator plus local worktree workers; one defect per worker branch and checkpoint commit.

## Ground Truth Files

- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `.agent/NEXT_STEPS.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

## Baseline Validation

- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21 tests, 2026-06-12 before this branch.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests, 2026-06-12 before this branch.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21 tests, 2026-06-12T23:01Z on this branch after harness bootstrap files.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests, 2026-06-12T23:01Z on this branch after harness bootstrap files.
- `PHASES.md`: expanded, 2026-06-12T23:19Z, to include local-only phase run units, agent-team handoffs, golden-corpus fix discipline, and the 45 structural-test gate.
- P0 preflight, 2026-06-12T23:21Z: `agh doctor`, `agh --version`, `ghc --version`, `lake --version`, and `lean --version` all exited 0. Raw logs in `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/`.
- P0 first RUN, 2026-06-12T23:24Z: `regression-pr54-weyl` failed before review start because installed `/Users/mlong/.cargo/bin/grokrxiv-app` rejects `--loop`; classified P0-001 / F3.
- P0-001 fix, 2026-06-12T23:27Z: reinstalled `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run accepted `--loop` and emitted the review-loop stage plan.
- P0 second RUN, 2026-06-12T23:47Z: `regression-pr54-weyl` completed as review `eca527eb-3930-49e6-a828-66dd64611430`; review-loop deterministic status failed and opened PR #55. New findings: P0-002 no-publishing guardrail breach, P0-003 N1 extraction gate failure, P0-004 citation waterfall gap, P0-005 PR fixer timeout.

## Coordinator Rules

- Persist state in files, not chat memory.
- Do not weaken `expected:` blocks or `never_events` to make red runs green.
- Do not invoke `approve`, `request-revisions`, publisher, or merge actions from corpus loop sessions.
- Stop immediately on N5 and write a human escalation dossier.
- End every local session with state updates, ledger append, `git status`, and a checkpoint commit.
