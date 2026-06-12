# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 2: fix P0-001 first. Use local Codex only; do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

Review the P0-001 dossier:
- .agent/FINDINGS.md
- agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier.md

Install current local product binaries:
- cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked
- cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked

Re-run the exact product command:
- agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json

If the product command starts the review-loop, continue LOOP.md RUN+CHECK for regression-pr54-weyl and classify the next failure F1-F5.
If it still fails before review start, add adapter/runtime product-surface coverage and fix the resolution path before moving on.
After the CLI runner starts successfully, lock the exact local `api` runner command in LOOP.md or PHASES.md before making any two-runner green claim.
Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
