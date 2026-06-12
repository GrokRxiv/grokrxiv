# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 1: audit only. Do not patch.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

Run LOOP.md preflight:
- agh doctor
- ghc --version
- lake --version
- lean --version
- record contract SHAs for app.yaml, dags/, agents/, prompts/, schemas/

Lock the exact local api-runner command in LOOP.md or PHASES.md if it is not already unambiguous.

Run RUN+CHECK against agenthero/apps/grokrxiv/evals/corpus.yaml, starting with regression-pr54-weyl.
Classify every failure F1-F5.
Write dossiers to .agent/FINDINGS.md.
Write ordered fixes to .agent/PATCH_PLAN.md.
Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
