# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-035 Coordinator Merge

Current worker:
- Branch: `p0-035-haskell-author-timeout`
- Worktree: `.agent/worktrees/p0-035-haskell-author-timeout`
- Base: coordinator `107bcba`; worker now includes deterministic Haskell authoring, scaffold obligation filtering, and truncated-statement semantic-gap handling.
- Status: accepted by normal wrapped CLI affected rerun; commit this worker and merge it into `grokrxiv-local-corpus-harness`.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

Accepted evidence:
- Normal CLI affected rerun result dir: `agenthero/apps/grokrxiv/evals/results/20260613T181916Z/regression-pr54-weyl-cli-after-p0-035-truncated-gap`.
- Review id: `e97e30a8-08ba-4741-a7f4-d3e4b5ee2a75`.
- Product `run.log`: `ok=true`, `output.status=0`.
- Wrapper note: `exit.status=0`, `wrapper.status=1`, and `STATUS_RECOVERY.md` document that only the local zsh wrapper failed after product completion because `status=$?` is read-only in zsh.
- External actions stayed disabled and `pr_url=null`.
- Haskell passed in one deterministic attempt: `generation_recovery.status=deterministic_local_author`, pinned GHC compile exit 0, semantic validation pass, independent reviewer pass.
- Proof obligations generated: `theorem_obligations=10`.
- Citation remained within Tier R: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`.

Worker verification already passed:

```bash
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib -- --nocapture
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop --lib -- --nocapture
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
git diff --check
cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --force --locked
cargo install --path agenthero/apps/grokrxiv/rust --force --locked
```

Before merge, run the 45 structural tests from this worker:

```bash
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
```

Then checkpoint and merge:

```bash
git status --short --branch
git add .
git commit -m "codex checkpoint: P0 - Haskell CLI acceptance"
cd /Users/mlong/Documents/Development/grokrxiv
git merge --ff-only p0-035-haskell-author-timeout
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop --lib
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
git diff --check
git status --short --branch
```

## Next Defect

After P0-035 merge, queue P0-036 from the next red in the accepted Tier R run:

- `pr_artifact_fixer` timed out after 360s despite prior P0-005 fast-path work. This is mechanically bounded and app-local if it reproduces.
- Lean remains `NOT_PROVED`/`FAILED` and semantic adequacy remains `OVERCLAIMED`; classify as F2/P2 architecture unless a narrow P0 bug is found.

Do not use Codex Cloud, cloud apply, or cloud task state.
Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
