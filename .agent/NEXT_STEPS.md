# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-043 Worker Checkpoint

Current worker:
- Branch: `p0-043-zeta-citation-timeout`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-043-zeta-citation-timeout`
- Base commit: `83c403b`
- Status: source fix and state updates are ready for checkpoint commit, then coordinator fast-forward merge.

P0-043 worker evidence:
- App bug fixed: TeX `\bibitem` parsing now extracts the first bibliographic `\newblock` title instead of using the bibitem key as `Citation.title`.
- Red-first test failed before implementation and passed after the fix:
  - `bibitem_newblock_title_uses_bibliographic_title_not_key`
- Verification passed: focused fixture, ingest lib 47/47, app workspace check, structural tests 45/45, `git diff --check`, PATH installs for `grokrxiv-app` and `agenthero-dag-app-grokrxiv`, and installed safe dry-run.
- Affected no-cache rerun `20260613T230107Z/zeta3-after-p0-043-bibitem-titles` completed as review `c393d134-a7e1-4275-bbde-4d85cbfb63c4`.
- External actions stayed disabled and `pr_url=null`.
- Versioned references now have `key_title_count=0`.
- Citation validation is now non-blocking warning: `checked=32`, `unverified=5`, `unresolved=0`, `transient_unknown=0`.
- Policy gate no longer has a citation-validation blocking issue.

Expected next session shape:
1. Commit the P0-043 worker:
   `git commit -m "codex checkpoint: P0 - fix zeta bibitem citation titles"`.
2. Return to the coordinator worktree:
   `cd /Users/mlong/Documents/Development/grokrxiv`.
3. Fast-forward merge the worker branch:
   `git merge --ff-only p0-043-zeta-citation-timeout`.
4. Re-run coordinator verification:
   - `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest --lib`
   - `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`
   - `cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract`
   - `git diff --check`
5. Update `.agent/AGENT_STATUS.md`, `.agent/TEST_LOG.md`, and `agenthero/apps/grokrxiv/evals/results/LEDGER.md` with coordinator verification, then checkpoint commit if those state files change.
6. After merge, choose the next thread:
   - ask for human decision on `bertrand-elementary` pinned `2407.07620v5`; or
   - if no human corpus decision is available, start P0-044 in a fresh worker.

## P0-044 Candidate

Name: zeta Haskell semantic target hygiene / bibliography snippets

Trigger:
- After P0-043, zeta citation validation no longer blocks policy.
- The affected rerun remains red because Haskell/Lean/semantic adequacy block on partial proof obligations from bibliography/math snippets such as `body_math_41` and `body_math_67`.

Expected defect loop:
1. Create a fresh worker from the coordinator after P0-043 merge.
2. Add a failing fixture proving bibliography/reference math snippets and `SemanticGap`/`StatusPartial` entries do not become required Haskell/Lean proof obligations.
3. Fix the app-owned semantic target selection, not by raising timeouts or weakening corpus expectations.
4. Rerun `zeta3-irrationality` safely with `--no-external-actions`.
5. Commit the worker and merge only after focused tests, app workspace check, structural tests, and `git diff --check` pass.

## P0-039 Human Corpus Decision

Current status:
- App-local arXiv version preservation is fixed and merged at `1aeab11`.
- `bertrand-elementary` remains blocked because the corpus pins `2407.07620v5`, which is withdrawn/unavailable, while `expected.extraction=full_body`.
- v1-v4 are retrievable.

Allowed human decisions:
- approve changing corpus pin from `v5` to latest retrievable `v4`, then rerun safely;
- replace the Tier A Bertrand entry with a retrievable source;
- or explicitly change expected extraction semantics for withdrawn `v5`.

Do not edit the corpus version or expected block without explicit sign-off.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
- Do not tag P0 green; a full corpus/both-runner sweep is still required.
