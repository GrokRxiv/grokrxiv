# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-042 PR Deterministic Fast-Path Miss

Current worker:
- Branch: `p0-041-render-quantifier-escape`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-041-render-quantifier-escape`
- Status: ready to checkpoint, merge to coordinator, and run coordinator verification.

After P0-041 merge:
- Coordinator branch: `grokrxiv-local-corpus-harness`
- Coordinator worktree: `/Users/mlong/Documents/Development/grokrxiv`
- Start a fresh worker branch/worktree, for example `p0-042-pr-deterministic-fast-path`.

P0-041 evidence:
- Result root: `agenthero/apps/grokrxiv/evals/results/20260613T212629Z/zeta3-after-p0-041-quantifiers`
- Review id: `2f24f79c-a592-4490-926c-a3f093abe1b1`
- Product exit: `0`
- External actions: disabled; `pr_url=null`
- Fixed by P0-041: no `Unicode character ∃`, `Unicode character ∀`, `U+2203`, `U+2200`, raw `∃`, or raw `∀` failure remains in `review_loop/fixed/review.log`; fixed `review.pdf` was written.
- Residual red 1: citation specialist timed out after 360s, citation validation failed deterministic policy with `unverified=24`; no full corpus-green claim.
- Residual red 2, next defect: `review_loop/pr_fixes.json` has `status=pass` and first compile exit 0, but still reports `compile_review_loop.author_role=pr_artifact_fixer`, `compile_review_loop.agent_output_audit_summary.total=2`, and recovered on-disk output after `CliRunner timed out after 360s for role pr_artifact_fixer`. That means an already-compilable rendered artifact still invoked the timeout-prone LLM PR fixer/reviewer path.

Expected next session shape:
1. Commit and merge P0-041 if not already merged; run coordinator verification.
2. Start fresh worker `p0-042-pr-deterministic-fast-path` from the verified coordinator.
3. Add a red-first app-runtime/review-loop fixture proving that a rendered review which compiles on the first deterministic PR attempt records `author_role=deterministic_pr_artifact_compiler` and zero PR-fixer agent outputs, including the live path that currently recovers from on-disk output after `pr_artifact_fixer` timeout.
4. Fix the PR artifact stage so successful compile-first bypasses `pr_artifact_fixer` and `pr_artifact_reviewer` entirely instead of running the LLM path and accepting recovered files.
5. Run focused test, app-runtime `review_loop`, app workspace check, structural tests, and `git diff --check`.
6. Reinstall `grokrxiv-app` and `agenthero-dag-app-grokrxiv` from the worker.
7. Re-run `zeta3-irrationality` safely with `--no-external-actions`; verify `pr_fixes.json` has deterministic author role, zero agent outputs, compile exit 0, fixed PDF, and no Unicode errors.
8. Keep P0-039 Bertrand extraction failure queued separately; do not tag P0 green.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
