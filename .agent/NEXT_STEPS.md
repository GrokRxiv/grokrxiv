# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 23: continue local-only P0 from the P0-023 toolchain/corpus-pin checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- agenthero/apps/grokrxiv/evals/toolchain.lock.yaml
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

If branch `p0-023-toolchain-corpus-pins` has not yet been merged, first fast-forward merge it into `grokrxiv-local-corpus-harness` and rerun:

cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
git diff --check
git status --short

Current state:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- P0-023 corpus/toolchain pins are in repo state: all arXiv entries have concrete `vN` versions; `evals/toolchain.lock.yaml` pins GHC 9.14.1, Lean 4.30.0, Lake 5.0.0-src+d024af0, and mathlib v4.30.0 commit `c5ea00351c28e24afc9f0f84379aa41082b1188f`; `evals/lean/lake-manifest.json` records the resolved mathlib/transitive dependency set.
- Latest affected run remains `20260613T080031Z`, review `d18f023f-d9ce-4788-b81c-de7f3ba57c16`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- Citation report remains within Tier R threshold: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`.
- Paper math sources remain preserved: `paper_math_source_collector [OK] theorem_nodes=41 equations=903 sources=6 warnings=0`.
- PR fixer remains green: `review_loop/pr_fixes.json` has `status=pass`, `compile_review_loop.status=pass`, `author_role=deterministic_pr_artifact_compiler`, `agent_output_audit_summary.total=0`, and fixed artifacts `review_loop/fixed/review.tex` plus `review_loop/fixed/review.pdf`.
- Policy gate remains honest/non-publishing: `recommendation_policy.status=honest_non_publishing_recommendation`, `expected_recommendation=honest`, `actual_recommendation=major_revision`, `recommendation_policy.integrity_ready=true`, `publisher_ready=false`.
- No full corpus-green claim and no phase tag.

Next queue item: F3 GHC PATH drift before any phase-exit/full-corpus sweep.
- Repo pin requires `ghc --numeric-version` to resolve to `9.14.1`.
- Current shell returned `8.4.2` because `/usr/local/bin/ghc` precedes `/opt/homebrew/bin/ghc`.
- `/opt/homebrew/bin/ghc --numeric-version` returned `9.14.1`.
- Do not edit user shell startup files without approval. Either get explicit approval to fix PATH/symlinks, or configure an approved local runner environment so the preflight command itself records GHC 9.14.1.

After the GHC preflight is clean:
- Run the LOOP.md preflight again (`agh doctor`, contract SHAs, `ghc`, `lake`, `lean` versions).
- Run the next narrow corpus checks before a full sweep: the three synthetic entries with `--loop --debug --no-external-actions`, then Tier R if needed.
- Keep expected blocks/NEVER-events monotonic; do not weaken existing expectations.

Known red stages after the latest Tier R affected run:
- Haskell code-fixer timed out after 360s, so proof obligations and Lean were blocked by Haskell. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- Semantic adequacy remains `OVERCLAIMED`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
