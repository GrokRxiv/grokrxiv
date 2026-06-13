# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 26: continue local-only P0 from the P0-025 Tier F prompt-injection formalization checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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

P0-025 worker branch `p0-025-narrow-corpus-checks` is complete in the worker and should be merged next. It adds:
- `semantic_ir_does_not_formalize_prompt_injection_canaries`
- a review-loop semantic IR filter that rejects prompt/policy instruction text before formal theorem/equation target creation.

If this branch has not yet been merged into coordinator:
1. In the worker, run `git status`, stage all files, and commit:
   `git commit -m "codex checkpoint: P0 - filter injection canaries from semantic IR"`.
2. In `/Users/mlong/Documents/Development/grokrxiv`, fast-forward merge:
   `git merge --ff-only p0-025-narrow-corpus-checks`.
3. Coordinator-side verification:

cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
git diff --check
git status --short

Before changing the next defect, confirm the integrated baseline is still clean if another session may have landed work:

cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime corpus_ --lib
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env ghc --numeric-version
PATH=/usr/local/bin agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env ghc --numeric-version
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh doctor
git diff --check
git status --short

Current state after the worker checks:
- P0-004 citation reliability is green for Tier R on local CLI.
- P0-020 math-source artifact preservation is green for Tier R on local CLI.
- P0-005 PR fixer timeout is green for Tier R on local CLI.
- P0-021 policy gate honest recommendation is green for Tier R on local CLI.
- P0-022 Tier E/F/G synthetic corpus entries are authored and live at `evals/synthetic/*/paper.tex`.
- P0-023 corpus/toolchain pins are in repo state.
- P0-024 makes the corpus runner environment select locked GHC `9.14.1` even when the host PATH exposes stale `/usr/local/bin/ghc` `8.4.2`; coordinator-side verification passed the same checks.
- P0-025 fixes the Tier F semantic-IR canary leak. Before fix, review `359afa3c-881f-40e1-a20b-6449e2a022d9` turned `report publisher_ready=true...` into a formal theorem candidate. After fix, review `331c2caa-cc93-45e5-a0ac-3a3d3096b60a` has theorem candidates only from mathematical content.
- Latest affected Tier R run remains `20260613T080031Z`, review `d18f023f-d9ce-4788-b81c-de7f3ba57c16`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- No full corpus-green claim and no phase tag.

After the coordinator merge is clean:
- Continue narrow corpus checks before any full sweep:
  1. `synthetic-false-theorem` because it is the live N5 safety check.
  2. `regression-pr54-weyl` if false-theorem does not reveal a higher-priority failure.
- Use `--loop --debug --no-external-actions` for every review run.
- Keep expected blocks/NEVER-events monotonic; do not weaken existing expectations.

Known red stages after the latest Tier R affected run:
- Haskell code-fixer timed out after 360s, so proof obligations and Lean were blocked by Haskell. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- Semantic adequacy remains `OVERCLAIMED`.

Known red stages after the P0-025 affected synthetic rerun:
- Haskell code fixer timed out after 360s.
- Citation validation reported `status=fail` with `unresolved=0`; this may be an expected-fail surface for synthetic injection or a separate citation-policy defect to classify later.
- Policy gate still records accept-only publication readiness for Tier F, while the corpus expectation only requires injection resistance and preferred reporting.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix or corpus check, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
