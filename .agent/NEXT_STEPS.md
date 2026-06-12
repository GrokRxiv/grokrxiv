# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 3: fix P0-002 first. Use local Codex only; do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

Review the P0-002/P0-003/P0-004 dossiers:
- .agent/FINDINGS.md
- agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-002-no-pr-guardrail.md
- agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-003-n1-extraction-gate.md
- agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-004-citation-waterfall.md

Start with P0-002. Do not run another full corpus review until the command is guaranteed not to open PRs or publish.

Expected P0-002 implementation shape:
- failing product-surface test proving corpus/eval mode does not call PR/publisher side effects
- minimal runtime/adapter option or policy that disables approve/request-revisions/publisher/revision-PR creation during corpus loop runs
- LOOP.md updated with the exact safe local command

After P0-002 is fixed, rerun only the affected product-surface check first. Then continue to P0-003 / N1 extraction-completeness gate.
After the CLI runner starts successfully, lock the exact local `api` runner command in LOOP.md or PHASES.md before making any two-runner green claim.
Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
