# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 29: merge the P0-028 Tier R rerun dossier, then diagnose the local agent-runner empty failures. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Worker branch `p0-028-tier-r-regression-rerun` is ready for coordinator merge from:

cd /Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-028-tier-r-regression-rerun

It contains only durable state/ledger updates for the Tier R rerun, not app code changes.

P0-028 evidence:
- Run directory: `agenthero/apps/grokrxiv/evals/results/20260613T115145Z/regression-pr54-weyl/`
- Review ID: `3ccf7aa5-ce30-445f-8880-6fb4e15ad464`
- Product exit status: `0`
- External actions disabled; `pr_url=null`
- Extraction/math-source signal preserved: `body_chars=117245`, `theorem_nodes=41`, `equations=903`, `warnings=0`
- Citation validation stayed within Tier R threshold: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`
- Bundle completeness passed
- PR fixer and PR review passed
- Honest recommendation policy stayed fixed: `recommendation_policy.status="honest_non_publishing_recommendation"`
- Lean emitted `verdict="NOT_PROVED"` and `proof_status="SEMANTIC_GAP"` because Haskell failed

Current red:
- `summary`, `technical_correctness`, first `meta_reviewer`, and `haskell_semantic_author` local runner invocations failed with empty ``claude` exited with Some(1)` messages.
- Haskell failure cascaded into proof obligations, Lean, semantic adequacy, and policy.
- `claude --version` exits 0 (`2.1.177 (Claude Code)`), so the binary exists; the per-role invocation path still needs diagnosis.

Coordinator merge ritual:

cd /Users/mlong/Documents/Development/grokrxiv
git status --short
git merge --ff-only p0-028-tier-r-regression-rerun
git diff --check
git status --short
git add .agent/AGENT_STATUS.md .agent/NEXT_STEPS.md .agent/PATCH_PLAN.md .agent/TEST_LOG.md agenthero/apps/grokrxiv/evals/results/LEDGER.md
git commit -m "codex checkpoint: P0 - tier r regression rerun integration"

Next defect:
1. Create a fresh worker branch for P0-029.
2. Reproduce one failing role invocation outside the full corpus run, preferably from:
   `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/3ccf7aa5-ce30-445f-8880-6fb4e15ad464/review_loop/agent_outputs/haskell_review_fix_code/round_1/haskell_semantic_author/`
3. Capture the exact command, exit code, stdout, stderr, and any relevant environment/config discovered from the app runner. Do not rely on chat memory.
4. If the failure is deterministic and app-local, add a focused failing test or fixture and fix it.
5. If the failure is environment/auth/model-runner state, write an F3 dossier with concrete operator action and stop that defect thread; do not mask it by raising token caps or timeouts.
6. Rerun `regression-pr54-weyl` only after P0-029 is classified or fixed.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Full sweeps only when the patch plan says the phase might be done.
```
