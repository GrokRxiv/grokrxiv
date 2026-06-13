# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 19: continue local-only P0 from the P0-020 math-source preservation checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- Latest affected run: 20260613T053725Z, review `aa69e733-3f72-44e0-af25-136c2b5012b7`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- Citation report: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`. Remaining residues are both March references and are within the Tier R `<= 2` threshold.
- Paper math sources: `body_sections=8`, `body_chars=117245`, `equations=903`, `theorem_nodes=41`, `warnings=0`; no `not_loaded` reasons.
- No full corpus-green claim and no phase tag.

Next queue item: P0-005 PR fixer timeout.
- The latest affected run has complete extraction/review/citation inputs, but `pr_fixer` failed with `CliRunner timed out after 360s for role pr_artifact_fixer`.
- Add a focused fixture for the PR-fixer timeout path before changing behavior. Diagnose why `pr_artifact_fixer` is slow or non-terminating on valid review-loop inputs; do not blind-bump timeouts.
- Re-run the affected Tier R entry after the fix and require `review_loop/pr_fixes.json`, `review_loop/fixed/review.tex`, and the PR review artifacts to be explicit and bounded.

Known red stages after P0-004f:
- Haskell typed-IR/semantic validation fails. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- PR fixer times out after 360s on valid inputs; P0-005 is next.
- Policy gate requires `accept`; add a fixture for Tier R `expected.recommendation: honest` before changing behavior.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
