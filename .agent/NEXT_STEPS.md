# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-041 PR Render Unicode Quantifier Escape

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-040 worker branch: `p0-040-render-integer-symbol-escape`
- Status: P0-040 fixed raw `ℤ` escaping but the affected rerun exposed the next same-surface renderer gap, raw quantifier `∃`.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

P0-040 evidence:
- Worker result root: `.agent/worktrees/p0-040-render-integer-symbol-escape/agenthero/apps/grokrxiv/evals/results/20260613T204908Z/zeta3-after-p0-040-integer-symbol`
- Review id: `f4ae38c0-4902-4545-a697-3fd499595d4a`
- Product exit: `0`
- External actions: disabled; `pr_url=null`
- Fixed by P0-040: no `Unicode character ℤ`, `U+2124`, or raw `ℤ` failure remains in `review_loop/fixed/review.log`; fixed `review.pdf` was written.
- Remaining blocker: direct scratch compilation of the original rendered `review.tex` fails on `Unicode character ∃ (U+2203) not set up for use with LaTeX` at line 44. The same sentence also contains `∀ (U+2200)`, so cover both quantifiers.

Expected next session shape:
1. Fast-forward merge P0-040 into the coordinator if not already merged.
2. Start a fresh local worker branch/worktree, for example `p0-041-render-quantifier-escape`.
3. Add red-first renderer coverage for raw `∃` and `∀` in review evidence text.
4. Implement minimal PDFLaTeX-safe mappings in `agenthero/apps/grokrxiv/crates/render/src/latex.rs`.
5. Run render tests, app-runtime PR fast-path coverage, app workspace check, and structural tests.
6. Reinstall `grokrxiv-app` and `agenthero-dag-app-grokrxiv` from the worker.
7. Re-run `zeta3-irrationality` safely with `--no-external-actions`.
8. Keep P0-039 Bertrand extraction failure queued separately; do not tag P0 green.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
