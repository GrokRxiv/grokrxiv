# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 33: integrate P0-032, then run the affected Tier R regression safely. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Current state:
- Coordinator branch: `grokrxiv-local-corpus-harness`
- P0-032 worker branch: `p0-032-haskell-target-scope`
- P0-032 base commit: `66fd9ea`
- P0-032 worker commit should be the newest commit on the worker branch after session checkpoint.
- No baseline tag, no full corpus-green claim, and no phase tag yet.

P0-032 fix summary:
- Root cause: `build_semantic_ir_from_paper_math` promoted every extracted `equations.json` item to a required `formal_math` theorem candidate with a Lean target.
- Prior P0-031 artifact had 913 theorem candidates: 903 from `equations.json` and 10 from `theorem_graph.json`.
- Fix: extracted equations now remain in `supporting_equations` with `lean_eligible=false`; theorem candidates are reserved for theorem-like paper sources.
- Schema/contract updated: `semantic_ir.schema.json` declares `supporting_equations`; app-runtime contract test asserts the field.
- PATH `grokrxiv-app` was installed from P0-032 and a safe dry-run passed.

Integration steps:
1. In coordinator:
   `git status --short --branch`
   `git merge --ff-only p0-032-haskell-target-scope`
2. Re-run coordinator-side checks:
   `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib`
   `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_contract_files_define_formalization_policy_surface --lib`
   `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`
   `git diff --check`
3. Commit the coordinator state-only merge verification update if needed.

Affected rerun:
1. Use the corpus wrapper and safe external-action mode:
   `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions`
2. Save raw stdout/stderr/exit status under a new `agenthero/apps/grokrxiv/evals/results/<ts>/regression-pr54-weyl/`.
3. Verify:
   - product exit status
   - `external_actions.enabled=false` and `pr_url=null`
   - extraction/math-source signal still present
   - citation still `unverified <= 2`, `unresolved=0`, non-empty partial results
   - `semantic_ir.json` has no `theorem_candidates` sourced from `equations.json`
   - `supporting_equations` count reflects extracted equations
   - Haskell/Lean/semantic adequacy new top status
4. If the entry remains red, classify the new top failure from raw artifacts into F1-F5 before patching. Do not raise timeouts or weaken corpus expectations.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
Full sweeps only when the patch plan says the phase might be done.
```
