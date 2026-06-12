# GrokRxiv Local Harness Status

Updated: 2026-06-12T23:19:27Z

## Current State

- Goal: Multi-day phased local Codex build of the GrokRxiv review pipeline on AgentHero, gated by the golden corpus.
- Current phase: P0 stabilize.
- Session type: coordinator plan update.
- Branch/worktree: `grokrxiv-local-corpus-harness` in `/Users/mlong/Documents/Development/grokrxiv`.
- Branch base commit: `0f157da`.
- Baseline tag: none yet.
- Last green sweep: none yet.
- Current runner: local `cli` first; local `api` runner command must be locked during P0 audit before any two-runner green claim.
- In-flight defect: none; P0 session 1 audit is next.
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

## Coordinator Rules

- Persist state in files, not chat memory.
- Do not weaken `expected:` blocks or `never_events` to make red runs green.
- Do not invoke `approve`, `request-revisions`, publisher, or merge actions from corpus loop sessions.
- Stop immediately on N5 and write a human escalation dossier.
- End every local session with state updates, ledger append, `git status`, and a checkpoint commit.
