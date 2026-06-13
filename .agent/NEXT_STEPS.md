# GrokRxiv Local Harness Next Steps

Continue exactly from here:

## P0-039 App Fix Merge and Bertrand Corpus Decision

Current coordinator:
- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- P0-039 worker branch: `p0-039-bertrand-extraction-completeness`
- Status: P0-039 worker is ready to checkpoint and merge. It fixes app-local arXiv version preservation and records a human-signoff corpus blocker for `bertrand-elementary`.

P0-039 worker evidence:
- App bug fixed: `parse_arxiv_source` now preserves valid modern `vN` suffixes for review ingest.
- App bug fixed: arXiv abs metadata parsing rewrites unversioned `citation_pdf_url` to the requested historical version when one was supplied.
- Red-first tests failed before implementation and passed after the fix:
  - `abs_metadata_preserves_requested_pdf_version`
  - `arxiv_review_source_parsing_preserves_version_suffix`
- Verification passed: ingest lib 46/46, app-runtime `review_loop` 17/17, app workspace check, structural tests 45/45, `git diff --check`, PATH installs.
- Installed dry-run confirms `grokrxiv-app review https://arxiv.org/abs/2407.07620v4 --loop --debug --no-external-actions --dry-run --json` reports plan id `2407.07620v4`.

P0-039 evidence from the P0-037 sweep:
- Entry: `bertrand-elementary`
- Source: `https://arxiv.org/abs/2407.07620v5`
- P0-037 result root: `.agent/worktrees/p0-037-full-cli-sweep/agenthero/apps/grokrxiv/evals/results/20260613T193033Z`
- Symptom: product exited 1 at extraction completeness before review; `body.md` had 0 chars and `sections.json` had no body sections.
- Updated interpretation: N1 behaved correctly. The remaining corpus issue is that Tier A expects `full_body` for pinned `v5`, but live arXiv returns 404 for both PDF and e-print at `v5`; v1-v4 are retrievable. This is not safe to fix by editing corpus ground truth without human sign-off.

Expected next session shape:
1. In the P0-039 worker, run final `git status`, add files, and commit: `codex checkpoint: P0 - preserve arxiv version pins`.
2. Return to coordinator `/Users/mlong/Documents/Development/grokrxiv`, fast-forward merge `p0-039-bertrand-extraction-completeness`.
3. Re-run coordinator checks: ingest focused/full as appropriate, app-runtime parser/review-loop, app workspace check, structural tests, and `git diff --check`.
4. Ask for human decision on `bertrand-elementary`:
   - approve changing corpus pin from `v5` to latest retrievable `v4`, then rerun safely;
   - replace the Tier A Bertrand entry with a retrievable source;
   - or explicitly change expected extraction semantics for withdrawn `v5`.
5. If no human corpus decision is available, stop the P0-039 thread and continue only with a separate queued defect such as P0-043 zeta citation timeout.
6. Do not tag P0 green; a full corpus/both-runner sweep is still required.

Guardrails:
- Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
- Do not weaken `expected:` blocks or NEVER-events.
- Do not raise token caps or timeouts without a diagnosed cause.
- Keep structural tests green.
