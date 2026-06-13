# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## Current Coordinator State

- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- Latest coordinator checkpoint: P0-043 merged at `347d858`
- Status: P0-043 is merged and coordinator-verified.

P0-043 summary:
- TeX `\bibitem` parsing now extracts the first bibliographic `\newblock` title instead of using the bibitem key as `Citation.title`.
- Affected no-cache zeta rerun `20260613T230107Z/zeta3-after-p0-043-bibitem-titles` completed as review `c393d134-a7e1-4275-bbde-4d85cbfb63c4`.
- External actions stayed disabled and `pr_url=null`.
- Versioned references have `key_title_count=0`.
- Citation validation is now non-blocking warning: `checked=32`, `unverified=5`, `unresolved=0`, `transient_unknown=0`.
- Policy gate no longer has a citation-validation blocking issue.
- Coordinator verification passed: ingest lib 47/47, app workspace check, structural tests 45/45, `git diff --check`.

## Next Work Choice

### Option 1: P0-039 Human Corpus Decision

Current status:
- App-local arXiv version preservation is fixed and merged.
- `bertrand-elementary` remains blocked because the corpus pins `2407.07620v5`, which is withdrawn/unavailable, while `expected.extraction=full_body`.
- v1-v4 are retrievable.

Allowed human decisions:
- approve changing corpus pin from `v5` to latest retrievable `v4`, then rerun safely;
- replace the Tier A Bertrand entry with a retrievable source;
- or explicitly change expected extraction semantics for withdrawn `v5`.

Do not edit the corpus version or expected block without explicit sign-off.

### Option 2: P0-044 Zeta Haskell Semantic Target Hygiene

Trigger:
- After P0-043, zeta citation validation no longer blocks policy.
- The affected rerun remains red because Haskell/Lean/semantic adequacy block on partial proof obligations from bibliography/math snippets such as `body_math_41` and `body_math_67`.

Expected defect loop:
1. Create a fresh worker from the coordinator:
   `git worktree add .agent/worktrees/p0-044-zeta-haskell-target-hygiene -b p0-044-zeta-haskell-target-hygiene`
2. Add a failing fixture proving bibliography/reference math snippets and `SemanticGap`/`StatusPartial` entries do not become required Haskell/Lean proof obligations.
3. Fix the app-owned semantic target selection, not by raising timeouts or weakening corpus expectations.
4. Rerun `zeta3-irrationality` safely with `--no-external-actions`.
5. Commit the worker and merge only after focused tests, app workspace check, structural tests, and `git diff --check` pass.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
- Do not tag P0 green; a full corpus/both-runner sweep is still required.
