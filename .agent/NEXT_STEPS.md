# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 30: merge and verify P0-029. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- Worker branch `p0-029-agent-runner-empty-failure`
- Worker base `4f18357`
- Worker checkpoint commit is expected to be `codex checkpoint: P0 - classify cli stdout session limit`
- No baseline tag, no full corpus-green claim, and no phase tag yet

P0-029 result:
- The empty local `claude` exit-1 failure from P0-028 was diagnosed.
- With normal shell API env, the exact Haskell harness invocation succeeded.
- With app-equivalent provider API env scrubbing, a tiny Claude prompt exited 1 with structured stdout containing `api_error_status=429` and `You've hit your session limit`, while stderr was empty.
- Root cause in app code: `exec_and_capture` only inspected stderr on nonzero subprocess exits, so stdout-only structured provider failures became blank generic errors.
- Fix: `exec_and_capture` now combines stderr, stdout, and CLI log for quota/session-limit detection, and generic nonzero failures include bounded `stderr=`, `stdout=`, or `log=` detail.
- Regression test: `exec_and_capture_classifies_claude_session_limit_on_stdout`.

P0-029 verification already run in the worker:
- Red-first targeted test failed before the fix with `error chain should carry CliError for stdout session limits`.
- Targeted test passed after the fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime agents::runners::cli::tests --lib -- --nocapture`: pass, 42 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass.
- `grokrxiv-app --json --status review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions --dry-run`: pass.

Session 30 task:
1. In the worker, confirm `git diff --check`, `git status --short`, and commit:
   `git add .`
   `git commit -m "codex checkpoint: P0 - classify cli stdout session limit"`
2. In the coordinator worktree:
   `cd /Users/mlong/Documents/Development/grokrxiv`
   `git status --short --branch`
   `git merge --ff-only p0-029-agent-runner-empty-failure`
3. Run coordinator verification:
   `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime agents::runners::cli::tests --lib -- --nocapture`
   `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`
   `git diff --check`
4. If coordinator verification passes, append ledger/state updates and commit:
   `codex checkpoint: P0 - cli stdout session limit integration`
5. Only after merge verification, decide P0-031:
   - If local Claude scrubbed-env session limit has reset, rerun `regression-pr54-weyl`.
   - If not, configure a deliberate CLI quota fallback and test it before rerunning.
   - Do not raise token caps/timeouts, do not weaken `expected:` blocks, and do not claim phase exit.

Safe Tier R rerun command when runner is usable:
agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Full sweeps only when the patch plan says the phase might be done.
```
