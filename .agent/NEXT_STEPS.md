# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 27: continue local-only P0 from the P0-026 false-theorem corpus-liveness checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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

P0-026 worker branch `p0-026-false-theorem-n5-check` has been fast-forward merged into coordinator at `43bbf3a`. It added:
- a stronger `corpus_synthetic_entries_are_live_app_relative_manuscripts` test that parses synthetic TeX sources through review ingest and asserts parsed body length clears the 1,000-character extraction gate.
- a larger Tier G false-theorem manuscript that still preserves the false universal claim and explicit `n=40` counterexample.

Before changing the next defect, confirm the integrated baseline is still clean if another session may have landed work:

cd /Users/mlong/Documents/Development/grokrxiv
git status --short
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo test -p agenthero-orchestrator --test dag_app_registry --test agenthero_cli_contract
git diff --check
git status --short

Current state after the worker checks:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- P0-023 corpus/toolchain pins are in repo state.
- P0-024 makes the corpus runner environment select locked GHC `9.14.1` even when the host PATH exposes stale `/usr/local/bin/ghc` `8.4.2`.
- P0-025 fixes the Tier F semantic-IR canary leak; coordinator-side verification passed.
- P0-026 fixes the Tier G false-theorem fixture liveness defect. Before fix, `synthetic-false-theorem` failed at extraction with parsed body length 741. After fix, affected rerun `20260613T102058Z`, review `7ac26d88-9e8a-457f-bce0-a6425a42ad33`, reached review-loop theorem mapping with `theorem_candidates=2`. Coordinator-side verification passed.
- Latest affected Tier R run remains `20260613T080031Z`, review `d18f023f-d9ce-4788-b81c-de7f3ba57c16`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- No full corpus-green claim and no phase tag.

Next defect:
1. P0-027 false-theorem Lean verdict path. The Tier G run now reaches theorem mapping and N5 does not trigger, but the expected `lean_review_fix_code: NOT_PROVED` is still red. Actual evidence: `haskell_code_fixer` timed out after 360s, `proof_obligation_generator` failed, and `lean/results.json` has `skipped=true` with `skip_reason="Haskell mathematical IR generation did not pass; Lean verification is blocked."`
2. Decide whether P0 should emit an honest deterministic `NOT_PROVED`/blocked verdict for Haskell IR failures or whether this is the P2 typed-IR/deterministic-Lean architecture gap. Record the decision in `.agent/FINDINGS.md`; do not weaken `expected:`.
3. After the false-theorem verdict path is classified or fixed, rerun `regression-pr54-weyl` before any full sweep.

Known red stages:
- `synthetic-false-theorem`: Haskell code fixer timed out after 360s; Lean skipped; semantic adequacy `OVERCLAIMED`; policy/publish non-ready. N5 did not trigger.
- Latest Tier R: Haskell/Lean/semantic adequacy remain red; citation, PR fixer, and honest policy are green on local CLI.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the coordinator merge or next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
