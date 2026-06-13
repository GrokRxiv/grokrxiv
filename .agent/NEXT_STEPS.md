# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 35: diagnose and fix Haskell semantic-author timeout after the P0-034 proposition-fidelity guard. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-034 worker `p0-034-haskell-prop-fidelity` fast-forward merged at `212aaaf`
- P0-034 implemented deterministic Haskell proposition-fidelity validation and prompt/reviewer constraints.
- No baseline tag, no full corpus-green claim, and no phase tag yet.

P0-034 evidence:
- Red-first fixture `haskell_validator_rejects_raw_theorem_tautologies` failed before implementation, then passed.
- Verification passed:
  `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop haskell_validator_rejects_raw_theorem_tautologies --lib -- --nocapture`
  `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-review-loop --lib`
  `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`
  `git diff --check`
  `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`
- Affected rerun `20260613T134041Z` as review `2d695158-7d82-4242-8038-e62a37d3f928` reached Haskell round 2. The artifact no longer contained `PRaw` or `True /- raw`; it failed honestly on missing canonical Lean target declarations.
- Final affected rerun `20260613T140644Z` as review `d146096c-c34d-43d6-b7a2-251fe4919e67` completed with product exit 0, external actions disabled, `pr_url=null`, target scoping held (`theorem_candidates=10`, `supporting_equations=903`), citation stayed within Tier R threshold (`checked=53`, `unverified=1`, `unresolved=0`, `transient_unknown=0`), but `haskell_semantic_author` timed out after 360s before producing a module.
- Lean stayed `NOT_PROVED`/`SEMANTIC_GAP`; semantic adequacy stayed `OVERCLAIMED`.

Session 35 task:
1. Confirm the P0-034 integration checkpoint is present and clean:
   `git log --oneline -3`
   `git status --short --branch`
2. Create or continue a local worker for one defect only:
   `git worktree add .agent/worktrees/p0-035-haskell-author-timeout -b p0-035-haskell-author-timeout`
3. Reproduce the exact Haskell semantic-author invocation from:
   `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/d146096c-c34d-43d6-b7a2-251fe4919e67/review_loop/haskell/`
4. Capture exact command, model, exit code, stdout, stderr, input sizes, and decision artifacts. Classify the timeout before patching.
5. Add a failing fixture or harness test for the diagnosed trigger.
6. Fix narrowly. Preferred directions: reduce/structure Haskell author input, split oversized context, or make timeout failures produce actionable partial diagnostics. Do not raise timeouts or token caps without a diagnosed cause.
7. Run focused tests, app workspace check, `git diff --check`, install the PATH `grokrxiv-app` if runtime behavior changes, and rerun `regression-pr54-weyl` safely through:
   `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions`
8. Update `.agent/*`, append `LEDGER.md`, and checkpoint commit. Full sweeps only when the patch plan says P0 might be done.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not raise token caps or timeouts without a diagnosed cause.
```
## Next session - P0-035 resume after local Claude quota reset

Current worker:
- Branch: `p0-035-haskell-author-timeout`
- Worktree: `.agent/worktrees/p0-035-haskell-author-timeout`
- Base: coordinator `107bcba`; not merged yet.

First action:
1. Read `.agent/AGENT_STATUS.md`, `.agent/FINDINGS.md`, `.agent/PATCH_PLAN.md`, `.agent/TEST_LOG.md`, and `agenthero/apps/grokrxiv/evals/results/LEDGER.md`.
2. Confirm local Claude CLI quota reset with a scrubbed-env tiny prompt or use a deliberately tested API-runner path. Do not rerun the full Tier R entry while quota is exhausted.
3. Re-run the affected entry safely after reinstalling binaries from this branch:

```bash
cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --force --locked
cargo install --path agenthero/apps/grokrxiv/rust --force --locked
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env \
  agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 \
  --loop --debug --no-external-actions
```

Acceptance to integrate P0-035:
- `haskell_semantic_author` must not time out.
- Haskell attempt 1 should be `generation_recovery.status=deterministic_local_author`.
- `SemanticModel.hs` must compile under pinned GHC and preserve Lean declarations/source spans/typed conclusions.
- External actions remain disabled and `pr_url=null`.
- Citation remains within Tier R threshold (`needs_review`/unverified <= 2).

If the rerun passes the P0-035 acceptance but later stages remain red, checkpoint and merge this worker as the Haskell author-timeout fix, then queue the next distinct defect. If deterministic Haskell still fails local GHC or semantic validation, fix that before merging. Do not weaken corpus expectations or raise timeouts/token caps.
