# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-038 PR Render Unicode Sqrt Escape

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-037 worker checkpoint: pending merge from `p0-037-full-cli-sweep`
- Status: P0-037 audit exposed two reds and no phase tag exists.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

P0-037 evidence:
- Worker sweep root: `.agent/worktrees/p0-037-full-cli-sweep/agenthero/apps/grokrxiv/evals/results/20260613T193033Z`.
- Preflight: wrapped `agh doctor`, `agh --version`, `ghc --version`, `lake --version`, and `lean --version` all exited 0.
- Structural baseline: `cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract` passed 45/45 in worker.
- `bertrand-elementary`: exit 1 at extraction completeness; `run.log` records `no body sections` and `body text is too small for review context (0 chars)`. No review proceeded.
- `zeta3-irrationality`: review `bd8df0ab-3698-42c2-8f69-f7de7620cfee` reached PR artifact fixing; worker artifact log `.agent/worktrees/p0-037-full-cli-sweep/agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/bd8df0ab-3698-42c2-8f69-f7de7620cfee/review_loop/fixed/review.log` records `Unicode character √ (U+221A) not set up for use with LaTeX` at rendered TeX line 46. Coordinator aborted this entry before the LLM PR fixer could mask the deterministic compile-first failure.

Expected next session shape:
1. Commit/merge P0-037 audit state if not already merged.
2. Start a fresh local worker branch/worktree, for example `p0-038-render-sqrt-escape`.
3. Add red-first renderer coverage for raw `√` in review evidence text.
4. Implement the minimal PDFLaTeX-safe mapping in `agenthero/apps/grokrxiv/crates/render/src/latex.rs`.
5. Run render tests, app-runtime PR fast-path coverage, app workspace check, and structural tests.
6. Re-run `zeta3-irrationality` safely with `--no-external-actions`.
7. Keep P0-039 Bertrand extraction failure queued separately; do not tag P0 green.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
