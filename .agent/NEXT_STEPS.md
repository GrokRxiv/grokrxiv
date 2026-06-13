# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 31: decide whether the local runner is usable, then rerun the Tier R regression only if it is. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-029 worker `p0-029-agent-runner-empty-failure` fast-forward merged at `2e7961b`
- Coordinator-side runner tests passed 42/42
- Coordinator-side app workspace check passed
- State-only integration commit is pending from the current session
- No baseline tag, no full corpus-green claim, and no phase tag yet

P0-029 result:
- The runner no longer drops stdout-only Claude failure details on nonzero exits.
- `exec_and_capture` now classifies quota/session-limit output from stderr, stdout, or CLI logs.
- Generic nonzero runner failures now include bounded `stderr=`, `stdout=`, or `log=` detail.
- The remaining issue is environment state: app-equivalent provider API env scrubbing recently produced Claude stdout with `api_error_status=429` and `You've hit your session limit`.

Session 31 task:
1. Confirm the coordinator state-only integration commit was created:
   `git log --oneline -3`
   `git status --short --branch`
2. Probe scrubbed-env Claude cheaply before any corpus rerun. Use a tiny prompt and the same provider API env scrubbing behavior as the app runner. Capture exit code, stdout, and stderr in `.agent/p0-031-runner-probe/` or the next result directory.
3. If the probe still reports session limit or quota exhaustion, write an F3 note, append ledger/test-log rows, update NEXT_STEPS with the reset/fallback action, and stop this defect thread. Do not run the corpus.
4. If the probe succeeds, create a local worker branch for the affected rerun and run:
   `agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799v1 --loop --debug --no-external-actions`
5. Capture raw output under:
   `agenthero/apps/grokrxiv/evals/results/<ts>/regression-pr54-weyl/run.log`
   `agenthero/apps/grokrxiv/evals/results/<ts>/regression-pr54-weyl/exit.status`
   `agenthero/apps/grokrxiv/evals/results/<ts>/provenance.json`
6. Check the result against `evals/corpus.yaml` and `evals/LOOP.md`, especially:
   - no external actions and `pr_url=null`
   - no N1/N2/N3/N4/N5 NEVER-events
   - extraction/math-source signal preserved
   - all specialists complete or explicit failure artifacts
   - citation partial result non-empty and `needs_review <= 2`
   - Haskell/Lean/semantic adequacy status is honest and mechanically surfaced
7. Update `.agent/FINDINGS.md`, `.agent/PATCH_PLAN.md`, `.agent/TEST_LOG.md`, `.agent/NEXT_STEPS.md`, and `agenthero/apps/grokrxiv/evals/results/LEDGER.md`; commit one checkpoint.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Full sweeps only when the patch plan says the phase might be done.
```
