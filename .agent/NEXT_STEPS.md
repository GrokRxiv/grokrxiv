# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 4: fix P0-003 / N1 extraction-completeness gate. Use local Codex only; do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

P0-002 is fixed locally. Corpus review runs must use:

agh --json app run grokrxiv review <source> --loop --debug --no-external-actions

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.

Review the P0-003 dossier:
- .agent/FINDINGS.md
- agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-003-n1-extraction-gate.md

Start P0-003 with a failing fixture test that reproduces the empty extraction from review eca527eb-3930-49e6-a828-66dd64611430:
- body.md is 0 bytes
- sections.json has 0 sections
- equations.json has 0 equations
- theorem_graph.json has 0 nodes
- extraction_report.json marked the stages ok
- review_loop/paper_math_sources.json still let downstream loop stages run

Expected P0-003 implementation shape:
- app-owned extraction-completeness gate under agenthero/apps/grokrxiv/
- explicit failed/blocked artifact or gate result when body/sections/theorem extraction is empty for a math-heavy source
- review/policy path aborts before specialist/meta/policy/PR-fix stages when extraction completeness is not green
- affected unit/fixture tests pass
- rerun only the affected regression entry with the safe command after the fixture fix is in place

After P0-003, continue top-down in .agent/PATCH_PLAN.md. Full sweeps only when the plan says the phase might be done.

Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
