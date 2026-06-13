# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-035 CLI Acceptance And Coordinator Merge

Current worker:
- Branch: `p0-035-haskell-author-timeout`
- Worktree: `.agent/worktrees/p0-035-haskell-author-timeout`
- Base: coordinator `107bcba`; worker checkpoint includes deterministic Haskell authoring plus scaffold obligation filtering; not merged yet.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

Current evidence:
- API override rerun `dad9153a-778c-4c4b-b2f3-f096a4c0ed21` proves P0-035 at the Haskell stage: attempt 1 used `deterministic_local_author`, pinned GHC compile passed, independent Haskell reviewer passed, and `theorem_obligations=10`.
- External actions stayed disabled and `pr_url=null`.
- Citation stayed within Tier R threshold: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`.
- This is not a full Tier R green claim: API novelty lacks a registered `gemini` provider, Lean remains `NOT_PROVED`/`FAILED`, semantic adequacy remains `OVERCLAIMED`, and normal CLI is still quota-blocked.

First actions:
1. Confirm local Claude CLI quota reset with a scrubbed-env tiny prompt. Do not rerun the full Tier R CLI entry while quota is exhausted.
2. If quota has reset, reinstall binaries from this branch and run the normal wrapped CLI affected rerun:

```bash
cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --force --locked
cargo install --path agenthero/apps/grokrxiv/rust --force --locked
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env \
  agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 \
  --loop --debug --no-external-actions
```

Acceptance to integrate P0-035:
- `haskell_semantic_author` must not time out.
- Haskell attempt 1 should be `generation_recovery.status=deterministic_local_author`.
- `SemanticModel.hs` must compile under pinned GHC and preserve Lean declarations/source spans/typed conclusions.
- External actions remain disabled and `pr_url=null`.
- Citation remains within Tier R threshold (`needs_review`/unverified <= 2).

If the rerun passes the P0-035 acceptance but later stages remain red, merge this worker as the Haskell author-timeout/scaffold-obligation fix, then queue P0-036 for Lean proof / semantic adequacy. If local Claude quota is still exhausted, keep this worker checkpointed and do not claim CLI acceptance.

Do not use Codex Cloud, cloud apply, or cloud task state.
Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
