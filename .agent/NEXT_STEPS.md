# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 20: continue local-only P0 from the P0-005 PR fixer checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

Corpus review runs must use:

agh --json app run grokrxiv review <source> --loop --debug --no-external-actions

Current state:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- Latest affected run: 20260613T072256Z, review `c0f0e300-2654-4e85-b26c-a50d530e24f0`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- Citation report: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`. Remaining residues are both March references and are within the Tier R `<= 2` threshold.
- Paper math sources: `paper_math_source_collector [OK] theorem_nodes=41 equations=903 sources=6 warnings=0`.
- PR fixer: `pr_fixer [OK]`, `pr_review_fix_code [OK]`; `review_loop/pr_fixes.json` has `status=pass`, `compile_review_loop.status=pass`, `author_role=deterministic_pr_artifact_compiler`, `agent_output_audit_summary.total=0`, and fixed artifacts `review_loop/fixed/review.tex` plus `review_loop/fixed/review.pdf`.
- No full corpus-green claim and no phase tag.

Next queue item: policy gate Tier R recommendation semantics.
- Add a focused fixture for `expected.recommendation: honest` before changing behavior.
- Current `policy_gate` requires meta-review recommendation `accept`; the Tier R corpus entry explicitly leaves the verdict unpinned and asserts review integrity rather than acceptance.
- Make the policy gate distinguish integrity-ready/honest-negative outcomes from publisher-ready accept outcomes without weakening NEVER-events or corpus expected blocks.
- Re-run the affected Tier R entry after the fix and require the policy artifact/report to show an honest non-publishing verdict rather than blocking solely because `recommendation=major_revision`.

Known red stages after P0-005:
- Lean proof-author timeout and semantic adequacy `OVERCLAIMED` remain. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- Policy gate requires `accept`; this is the next P0 item because Tier R only requires `expected.recommendation: honest`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
