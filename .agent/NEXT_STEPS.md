# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 28: run the next narrow corpus check locally. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Current coordinator state:
- Branch `grokrxiv-local-corpus-harness`
- P0-027 worker `p0-027-false-theorem-lean-verdict` fast-forward merged at `6ffc436`
- State-only integration commit is pending from the current session
- No baseline tag, no full corpus-green claim, and no phase tag yet

Integrated P0-027 evidence:
- Affected rerun `agenthero/apps/grokrxiv/evals/results/20260613T111624Z/synthetic-false-theorem-after-p0-027b/run.log`
- Review `5c2b0a1f-4ef8-4cba-96ae-16630b57931c`
- Product exit 0, external actions disabled, `pr_url=null`
- `lean_review_fix_code [FAIL] artifact=review_loop/lean/results.json status=fail verdict=NOT_PROVED proof_status=FAILED reason=review-fix-code loop did not prove the target`
- `review_loop/lean/theorem_map.json` has `status="FAILED"` and no `PROVED` entries
- Coordinator verification passed: review-loop crate 12/12, app review-loop 13/13, corpus tests 7/7, app workspace check, structural tests 45/45, `git diff --check`

Before starting the next defect, confirm the integration checkpoint is clean if another session may have landed work:

cd /Users/mlong/Documents/Development/grokrxiv
git status --short
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
git diff --check
git status --short

Next defect/check:
1. Rerun `regression-pr54-weyl` before any full sweep to verify Tier R did not regress after the synthetic-fixture and Lean-verdict changes.
2. Use the safe local runner and no external actions:

   agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions

3. Capture raw output under a new `agenthero/apps/grokrxiv/evals/results/<timestamp>/regression-pr54-weyl/` directory with `run.log` and an exit/status note, following LOOP.md.
4. Check Tier R expectations: full-body/theorem extraction remains present, all specialists explicit, citation partial result artifact non-empty, citation residue `needs_review`/unverified is `<= 2`, external actions disabled, PR fixer/policy regressions absent.
5. If Tier R remains green on citation/PR/policy and red only on typed-IR/Lean/semantic adequacy, classify the remaining P0/P2 boundary in `.agent/FINDINGS.md` without weakening `expected:`.
6. Full sweeps only when the patch plan says the phase might be done.

Known integrated state:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- P0-024 corpus runner selects locked GHC `9.14.1` even when host PATH exposes stale GHC.
- P0-025 fixes Tier F semantic-IR canary leak.
- P0-026 fixes Tier G false-theorem fixture liveness.
- P0-027 fixes the Tier G machine `NOT_PROVED` verdict path for failed/skipped Lean proof loops.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next narrow check or fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
