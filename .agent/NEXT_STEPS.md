# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 33: diagnose P0-032, the Haskell semantic target explosion / fixer timeout. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-031 worker `p0-031-tier-r-after-runner` fast-forward merged at `e7ebd4f`
- State-only integration commit is pending from the current session
- No baseline tag, no full corpus-green claim, and no phase tag yet

P0-031 evidence:
- Probe directory: `agenthero/apps/grokrxiv/evals/results/20260613T122028Z/p0-031-runner-probe/`
- Rerun directory: `agenthero/apps/grokrxiv/evals/results/20260613T122232Z/regression-pr54-weyl/`
- Review ID: `667842d3-71e0-4fe9-950a-1518db105049`
- Product exit status: `0`
- External actions disabled; `pr_url=null`
- Extraction/math-source signal preserved: `body_chars=117245`, `sections=8`, `theorem_nodes=41`, `equations=903`, `warnings=0`
- Citation validation stayed within Tier R threshold: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`
- PR fixer and PR review passed
- Honest recommendation policy stayed fixed
- Lean emitted `verdict="NOT_PROVED"` and `proof_status="SEMANTIC_GAP"` because Haskell failed

Current red:
- `semantic_category_mapper` emitted 913 theorem candidates for the Weyl paper.
- Haskell attempt 1 produced schema-valid output but was rejected: `SemanticModel.hs must include Lean target declaration thm_1`.
- Haskell attempt 2 timed out after 360s for role `haskell_code_fixer`.
- Semantic adequacy stayed `OVERCLAIMED` with 913 verdicts and empty emitted/verified statements.
- Policy/publish decision failed from the Haskell/Lean/semantic adequacy cascade.

Session 33 task:
1. Confirm the coordinator state-only integration commit was created:
   `git log --oneline -3`
   `git status --short --branch`
2. Start a fresh local worker from the coordinator:
   `git worktree add .agent/worktrees/p0-032-haskell-target-scope -b p0-032-haskell-target-scope`
3. Diagnose before patching:
   - Read `review_loop/semantic_model.json`, `semantic_ir.json`, `haskell/results.json`, and the Haskell decisions for review `667842d3-71e0-4fe9-950a-1518db105049`.
   - Locate the code that turns theorem/equation signals into Haskell/Lean target declarations.
   - Determine why equation targets are included as required Lean declarations and whether P0 should bound target selection or classify this as a P2 typed-IR gap.
4. If app-local, write a failing fixture first, then fix.
5. If architectural/P2, write an explicit F2/F4 dossier and keep the corpus red without weakening expectations.
6. Do not run a full Tier R rerun until the focused fixture or dossier makes the next action clear.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
Full sweeps only when the patch plan says the phase might be done.
```
