# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 28: merge and verify the P0-027 worker, then continue local-only P0. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Worker branch `p0-027-false-theorem-lean-verdict` is ready for coordinator merge from:

cd /Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-027-false-theorem-lean-verdict

It added:
- Lean proof-loop result annotation so `review_loop/lean/results.json` always exposes `verdict`, `proof_status`, and theorem-map `entries`.
- `verdict="PROVED"` only when the theorem map status is `PROVED`; all other failed/skipped proof-loop states are `verdict="NOT_PROVED"`.
- A theorem-map classifier fix so proof status is based on final generated Lean code, compile diagnostics, semantic-validation issue text, and skip/status fields, not reviewer prose.
- Red-first tests for skipped Lean `NOT_PROVED` annotation and reviewer-prose contamination.

Worker evidence:
- Affected rerun `agenthero/apps/grokrxiv/evals/results/20260613T111624Z/synthetic-false-theorem-after-p0-027b/run.log`
- Review `5c2b0a1f-4ef8-4cba-96ae-16630b57931c`
- Product exit 0, external actions disabled, `pr_url=null`
- `lean_review_fix_code [FAIL] artifact=review_loop/lean/results.json status=fail verdict=NOT_PROVED proof_status=FAILED reason=review-fix-code loop did not prove the target`
- `review_loop/lean/theorem_map.json` has `status="FAILED"` and no `PROVED` entries
- The entry remains red on semantic adequacy/citation/policy; no full corpus-green claim.

Merge ritual:

cd /Users/mlong/Documents/Development/grokrxiv
git status --short
git merge --ff-only p0-027-false-theorem-lean-verdict
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop -- --nocapture
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib -- --nocapture
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
git diff --check
git status --short

After coordinator verification, update .agent files and append LEDGER.md, then checkpoint:

git add .
git commit -m "codex checkpoint: P0 - false theorem lean verdict integration"

Next defect after merge:
1. Rerun `regression-pr54-weyl` before any full sweep to verify Tier R did not regress after the synthetic-fixture and Lean-verdict changes.
2. If Tier R remains green on citation/PR/policy and red only on typed-IR/Lean/semantic adequacy, classify the remaining P0/P2 boundary in `.agent/FINDINGS.md` without weakening `expected:`.
3. Full sweeps only when the patch plan says the phase might be done.

Known state:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- P0-024 corpus runner selects locked GHC `9.14.1` even when host PATH exposes stale GHC.
- P0-025 fixes Tier F semantic-IR canary leak.
- P0-026 fixes Tier G false-theorem fixture liveness.
- P0-027 fixes the Tier G machine `NOT_PROVED` verdict path for failed/skipped Lean proof loops.
- No baseline tag, no full corpus-green claim, and no phase tag yet.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
```
