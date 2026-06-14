# GrokRxiv Local Harness Next Steps

Continue exactly from here.

## Current Worker State

- Branch: `p0-044-zeta-haskell-target-hygiene`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-044-zeta-haskell-target-hygiene`
- Base checkpoint: coordinator `beddef4`
- Status: P0-044 code fix implemented and locally verified; affected rerun still pending because the first final rerun stalled before Haskell.

## P0-044 Summary

- Bibliography/reference sections are no longer scanned into theorem candidates.
- Deterministic Haskell author payload marks `claims`, `knowledge_graph`, `nonformal_review_claims`, `supporting_equations`, and raw `paper_math_sources` as omitted from code-author payload.
- Deterministic Haskell scaffold no longer defines `ReviewCategory` or imports nonformal review evidence into `ClaimIR`.
- `StatusPartial` / `SemanticGap` theorem candidates cannot emit proof obligations.
- Empty theorem candidates preserve `semantic_ir.limitations`, with empty `theoremTargets`, `claims`, and `allProofObligations`.
- Verification passed locally: focused red/green tests, `grokrxiv-review-loop` 16/16, app-runtime `review_loop` 19/19, app workspace check, structural tests 45/45, `git diff --check`, PATH installs, and installed dry-run.
- Affected rerun `20260613T235903Z/zeta3-after-p0-044-haskell-target-contract` was terminated as inconclusive/F3 after stalling before Haskell; no corpus-green or P0-044 acceptance claim.

## Exact Next Action

Rerun the affected zeta entry safely after confirming no stale child process remains:

```bash
cd /Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-044-zeta-haskell-target-hygiene
GROKRXIV_NO_CACHE=1 GROKRXIV_INGEST_NO_CACHE=1 \
  agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env \
  agh --json app run grokrxiv review https://arxiv.org/abs/2503.07625v2 \
  --loop --debug --no-external-actions
```

Acceptance for P0-044:

- run reaches `semantic_category_mapper` and `haskell_review_fix_code`;
- bibliography snippets such as prior `body_math_41`/`body_math_67` are absent from theorem candidates;
- if theorem candidates are empty, Haskell passes with explicit limitations and empty proof obligations, not backfilled review claims;
- if real theorem candidates are present, only `StatusTranscribed` non-`SemanticGap` formal math emits proof obligations;
- external actions remain disabled and `pr_url=null`.

After acceptance, update state files, commit the worker, then merge to coordinator and rerun coordinator-side checks.

## Faster Parallel Lanes

- Run P0-044 affected rerun acceptance in this worker.
- In a separate worker, add sweep-harness timeout/stall detection so stuck live runs classify as F3 quickly instead of burning wall-clock.
- Once the user signs off, run the P0-039 Bertrand v4/replacement corpus decision in another worker.

## Guardrails

- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
- Do not tag P0 green; a full corpus/both-runner sweep is still required.
