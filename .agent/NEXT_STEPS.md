# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 33: affected Tier R regression rerun after P0-032 semantic target scoping. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-032 worker `p0-032-haskell-target-scope` fast-forward merged at `2c64ac8`
- State-only integration commit is pending from the current session
- No baseline tag, no full corpus-green claim, and no phase tag yet

P0-032 fix summary:
- Root cause: `build_semantic_ir_from_paper_math` promoted every extracted `equations.json` item to a required `formal_math` theorem candidate with a Lean target.
- Prior P0-031 artifact had 913 theorem candidates: 903 from `equations.json` and 10 from `theorem_graph.json`.
- Fix: extracted equations now remain in `supporting_equations` with `lean_eligible=false`; theorem candidates are reserved for theorem-like paper sources.
- Schema/contract updated: `semantic_ir.schema.json` declares `supporting_equations`; app-runtime contract test asserts the field.
- PATH `grokrxiv-app` was installed from P0-032 and a safe dry-run passed.
- Coordinator verification after merge passed: review-loop crate tests 13/13, app-runtime contract test, app workspace check, and `git diff --check`.

Session 33 task:
1. Confirm the coordinator state-only integration commit was created:
   `git log --oneline -3`
   `git status --short --branch`
2. Start a fresh local worker from the coordinator:
   `git worktree add .agent/worktrees/p0-033-tier-r-after-target-scope -b p0-033-tier-r-after-target-scope`
3. Use the corpus wrapper and safe external-action mode:
   `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions`
4. Save raw stdout/stderr/exit status under a new `agenthero/apps/grokrxiv/evals/results/<ts>/regression-pr54-weyl/`.
5. Verify:
   - product exit status
   - `external_actions.enabled=false` and `pr_url=null`
   - extraction/math-source signal still present
   - citation still `unverified <= 2`, `unresolved=0`, non-empty partial results
   - `semantic_ir.json` has no `theorem_candidates` sourced from `equations.json`
   - `supporting_equations` count reflects extracted equations
   - Haskell/Lean/semantic adequacy new top status
6. If the entry remains red, classify the new top failure from raw artifacts into F1-F5 before patching. Do not raise timeouts or weaken corpus expectations.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
Full sweeps only when the patch plan says the phase might be done.
```
