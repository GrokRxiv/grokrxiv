# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-037 First Full Local CLI Corpus Sweep

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-036 merged checkpoint: `5152bf3`
- Status: P0-036 is merged and coordinator-side verification passed. No phase tag exists and no full P0 green claim has been made.

Read first:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

P0-036 evidence:
- Result dir: `agenthero/apps/grokrxiv/evals/results/20260613T185957Z/regression-pr54-weyl-after-p0-036-checkmark`.
- Review id: `752d5258-3821-433e-ae68-7ee8a150a8ad`.
- Product exit: `exit.status=0`, `run.log` has `ok=true` and `output.status=0`.
- External actions disabled, `pr_url=null`.
- `review_loop.status=pass`, `blocking_issues=[]`.
- `pr_fixes.status=pass`, `fixed_pdf=review_loop/fixed/review.pdf`, `compile_review_loop.author_role=deterministic_pr_artifact_compiler`, `agent_output_audit_summary.total=0`, compile exit 0.
- Haskell stayed green in one attempt.
- Lean reached `PROVED` on the affected Tier R rerun.
- Semantic adequacy reached `MATCHES`.
- Citation stayed within Tier R: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`.
- Policy integrity ready; publisher remains disabled/non-ready because the honest recommendation is `major_revision`.

Expected next session shape:
1. Start a fresh local worker branch/worktree, for example `p0-037-full-cli-sweep`.
2. Run LOOP preflight with `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env`.
3. Run all corpus entries locally with `--no-external-actions`.
4. Triage every red into F1-F5 with raw evidence and artifact paths.
5. Do not weaken `expected:` blocks or NEVER-events.
6. Do not tag P0 green unless the formal exit gate is met: two consecutive full-corpus sweeps, both runners, zero NEVER-events, phase expectations passing, and structural tests green.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
