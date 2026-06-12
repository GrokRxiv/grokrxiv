# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 1: audit only. Do not patch. Use local Codex only; do not use Codex Cloud, cloud apply, or cloud task state.

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
When PATCH_PLAN.md is ready, split work by PHASES.md Agent Teams:
- Gate Worker: N1-N5 only
- Citation Worker: resolver/retraction/partial-result program
- IR / Proof Worker: typed IR and Lean fidelity work when P2 opens
- Platform Worker: root-crate work only for P1/P3/P4/P5
Each worker uses a local worktree under .agent/worktrees/ and takes exactly one defect.
Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
