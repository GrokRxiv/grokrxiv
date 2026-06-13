# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-040 PR Render Unicode Integer-Symbol Escape

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-038 worker branch: `p0-038-render-sqrt-escape`
- Status: P0-038 fixed raw `√` escaping but affected rerun exposed the next same-surface renderer gap, raw `ℤ`.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

P0-038 evidence:
- Worker result root: `.agent/worktrees/p0-038-render-sqrt-escape/agenthero/apps/grokrxiv/evals/results/20260613T201053Z/zeta3-after-p0-038-sqrt`
- Review id: `82be001c-ffaf-47d4-820d-da0c7777c178`
- Product exit: `0`
- External actions: disabled; `pr_url=null`
- Fixed by P0-038: no `Unicode character √` failure remains in `review_loop/fixed/review.log`.
- New blocker: `.agent/worktrees/p0-038-render-sqrt-escape/agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/82be001c-ffaf-47d4-820d-da0c7777c178/review_loop/fixed/review.log` records `Unicode character ℤ (U+2124) not set up for use with LaTeX` at rendered TeX line 58, followed by no output PDF. `pr_fixes.json` records fallback into `pr_artifact_fixer`, which timed out after 360s.

Expected next session shape:
1. Fast-forward merge P0-038 into the coordinator if not already merged.
2. Start a fresh local worker branch/worktree, for example `p0-040-render-integer-symbol-escape`.
3. Add red-first renderer coverage for raw `ℤ` in review evidence text.
4. Implement the minimal PDFLaTeX-safe mapping in `agenthero/apps/grokrxiv/crates/render/src/latex.rs`.
5. Run render tests, app-runtime PR fast-path coverage, app workspace check, and structural tests.
6. Reinstall `grokrxiv-app` and `agenthero-dag-app-grokrxiv` from the worker.
7. Re-run `zeta3-irrationality` safely with `--no-external-actions`.
8. Keep P0-039 Bertrand extraction failure queued separately; do not tag P0 green.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
