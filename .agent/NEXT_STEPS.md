# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-039 Tier A Bertrand Extraction Completeness

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-042 worker branch: `p0-042-pr-deterministic-fast-path`
- Status: P0-042 is merged at `7240b2d` and coordinator verification passed. Start a fresh worker branch/worktree, for example `p0-039-bertrand-extraction-completeness`.

P0-042 evidence:
- Result root: `agenthero/apps/grokrxiv/evals/results/20260613T220435Z/zeta3-after-p0-042-nr-symbols`
- Review id: `21dd04be-2bc6-475c-9621-c877aefc9db8`
- Product exit: `0`
- External actions: disabled; `pr_url=null`
- Fixed by P0-042: original rendered zeta review no longer fails deterministic PR compile-first on raw `ℕ`/`ℝ`; `pr_fixes.json` has `author_role=deterministic_pr_artifact_compiler`, zero PR-fixer agent outputs, first compile exit 0, and fixed PDF written.
- Residual red: citation specialist timed out after 360s and citation validation failed deterministic policy with `checked=32`, `unverified=24`, `unresolved=0`, `transient_unknown=0`. Reconfirm in the next sweep; if repeated, queue a separate citation-timeout/evidence defect. Do not raise timeouts blindly.

P0-039 evidence from the P0-037 sweep:
- Entry: `bertrand-elementary`
- Source: `https://arxiv.org/abs/2407.07620v5`
- P0-037 result root: `.agent/worktrees/p0-037-full-cli-sweep/agenthero/apps/grokrxiv/evals/results/20260613T193033Z`
- Symptom: product exited 1 at extraction completeness before review; `body.md` had 0 chars and `sections.json` had no body sections.
- Interpretation: N1 behaved correctly by stopping review, but Tier A expects `full_body`; this is an extraction/source staging defect, not a gate defect.

Expected next session shape:
1. Start fresh worker `p0-039-bertrand-extraction-completeness` from the verified coordinator.
2. Inspect the P0-037 Bertrand result artifacts, data-repo artifacts, and source archive staging for `2407.07620v5`.
3. Add a red-first fixture that reproduces the empty Bertrand body/sections failure without a live corpus run.
4. Fix the app-local extraction path so Bertrand reaches a non-empty reviewable body and section list without weakening extraction completeness.
5. Run focused ingest/extraction tests, app-runtime review/extraction gate tests, app workspace check, structural tests, and `git diff --check`.
6. Reinstall `grokrxiv-app` and `agenthero-dag-app-grokrxiv` from the worker.
7. Re-run `bertrand-elementary` safely with `--no-external-actions`; verify extraction completeness reaches the review context expected by Tier A.
8. Do not tag P0 green; a full corpus/both-runner sweep is still required.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
