# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-036 Next Tier R Red Triage

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-035 merge: `1caf62d`
- Status: P0-035 is merged and coordinator verification passed.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

Accepted P0-035 evidence:
- Normal CLI affected rerun result dir: `agenthero/apps/grokrxiv/evals/results/20260613T181916Z/regression-pr54-weyl-cli-after-p0-035-truncated-gap`.
- Review id: `e97e30a8-08ba-4741-a7f4-d3e4b5ee2a75`.
- Product `run.log`: `ok=true`, `output.status=0`.
- External actions disabled, `pr_url=null`.
- Haskell `status=pass` in one deterministic attempt, `generation_recovery.status=deterministic_local_author`, pinned GHC compile exit 0, semantic validation pass, independent reviewer pass.
- Proof obligations generated: `theorem_obligations=10`.
- Citation remained within Tier R: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`.

Coordinator verification already passed after merge:

```bash
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop --lib
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
```

Next defect selection:
1. Reproduce and diagnose `pr_artifact_fixer` timeout from review `e97e30a8-08ba-4741-a7f4-d3e4b5ee2a75`. Prior P0-005 should have used a deterministic compile-first fast path, so this is the most mechanically bounded app-local residual.
2. If PR fixer no longer reproduces or is secondary, write a dossier for Lean `NOT_PROVED`/`FAILED` and semantic adequacy `OVERCLAIMED`. Treat this as F2/P2 architecture unless there is a narrow P0 regression in statement/gap classification.

Start P0-036 in a fresh local worker branch/worktree. Do not use Codex Cloud.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
