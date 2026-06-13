# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 34: fix Haskell semantic IR proposition fidelity. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Current worker state:
- Branch `p0-033-tier-r-after-target-scope`
- Base commit `2a6352d`
- P0-033 affected Tier R rerun completed as review `4bd37a7a-9452-476b-911d-9d75cfc37c51`
- Raw run directory: `agenthero/apps/grokrxiv/evals/results/20260613T130722Z/regression-pr54-weyl/`
- Artifact root: `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/4bd37a7a-9452-476b-911d-9d75cfc37c51/review_loop/`
- No baseline tag, no full corpus-green claim, and no phase tag yet

P0-033 evidence:
- Product exit status: `0`
- External actions disabled; `pr_url=null`
- Extraction/math-source signal preserved: `theorem_nodes=41`, `equations=903`
- P0-032 target scoping held live: `semantic_ir.json` has 10 theorem candidates, all from `theorem_graph.json`; `supporting_equations` has 903 entries, all from `equations.json`
- Citation remains within Tier R threshold: `checked=53`, `unverified=1`, `unresolved=0`, `transient_unknown=0`
- Haskell round 2 compiled with GHC exit 0 and semantic validation `pass`
- Independent `haskell_code_reviewer` still failed the artifact because `renderProp` renders `PRaw` as `True /- raw: ... -/`, and `paperTheoremClaim` gives paper theorem candidates empty binders and assumptions
- Lean is blocked with `verdict=NOT_PROVED` and `proof_status=SEMANTIC_GAP`
- Semantic adequacy is `OVERCLAIMED`

Session 34 task:
1. Confirm this P0-033 checkpoint commit exists:
   `git log --oneline -3`
   `git status --short --branch`
2. Create or continue a local worker for one defect only:
   `git worktree add .agent/worktrees/p0-034-haskell-prop-fidelity -b p0-034-haskell-prop-fidelity`
3. Add a failing fixture that rejects Haskell semantic artifacts where required paper theorem candidates are represented as tautological raw propositions (`PRaw -> True`), metadata-only Lean comments, or empty theorem binders/assumptions when the semantic IR carries theorem-level source spans.
4. Tighten the Haskell author/fixer prompt and deterministic semantic validation/review contract so unknown theorem content becomes an explicit semantic gap or uninterpreted predicate with provenance, never a proof-irrelevant tautology.
5. Preserve safety: if faithful theorem statements cannot be emitted, Lean must stay `NOT_PROVED`/`SEMANTIC_GAP`; do not convert failures into `PROVED`.
6. Run focused tests, app workspace check, `git diff --check`, install the PATH `grokrxiv-app` if CLI-facing behavior changes, and rerun `regression-pr54-weyl` safely through:
   `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions`
7. Update `.agent/*`, append `LEDGER.md`, and checkpoint commit. Full sweeps only when the patch plan says P0 might be done.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
```
