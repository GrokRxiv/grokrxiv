# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 22: continue local-only P0 from the P0-022 synthetic-corpus checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- Latest affected run: 20260613T080031Z, review `d18f023f-d9ce-4788-b81c-de7f3ba57c16`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- Citation report: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`. Remaining residues are both March references and are within the Tier R `<= 2` threshold.
- Paper math sources: `paper_math_source_collector [OK] theorem_nodes=41 equations=903 sources=6 warnings=0`.
- PR fixer: `pr_fixer [OK]`, `pr_review_fix_code [OK]`; `review_loop/pr_fixes.json` has `status=pass`, `compile_review_loop.status=pass`, `author_role=deterministic_pr_artifact_compiler`, `agent_output_audit_summary.total=0`, and fixed artifacts `review_loop/fixed/review.tex` plus `review_loop/fixed/review.pdf`.
- Policy gate: `policy_gate.json` has `recommendation_policy.status=honest_non_publishing_recommendation`, `expected_recommendation=honest`, `actual_recommendation=major_revision`, `recommendation_policy.integrity_ready=true`, `publisher_ready=false`; the accept-only meta-review reason is not in `blocking_issues`.
- Installed PATH dry-runs for `synthetic-bad-citations`, `synthetic-injection`, and `synthetic-false-theorem` resolved as local `Tex` sources with `external_actions.enabled=false`. Full synthetic review sweeps were not run in P0-022.
- No full corpus-green claim and no phase tag.

Next queue item: toolchain and corpus pins.
- Pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.
- Add fixtures or mechanical checks before implementation where practical: corpus entries with `version: pin_on_first_run` must be discoverable, pinned versions must not silently drift, and toolchain versions must be recorded in provenance.
- Keep expected blocks/NEVER-events monotonic; do not weaken existing expectations.
- If toolchain pinning exposes environment F3 failures, write the dossier and keep them separate from P2 typed-IR/Lean architecture work.

Known red stages after P0-021:
- Haskell code-fixer timed out after 360s in the latest affected run, so proof obligations and Lean were blocked by Haskell. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- Semantic adequacy remains `OVERCLAIMED`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
