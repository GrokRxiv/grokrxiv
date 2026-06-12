# GrokRxiv Local Harness Patch Plan

P0 audit has raw evidence for the first regression entry. Work this queue top-down, one defect per local session or worker branch.

## Seeded Queue

1. Coordinator / Platform Worker: P0-002 no-publishing guardrail. Add a safe local corpus/eval mode for `agh app run grokrxiv review ... --loop` that cannot open PRs or publish, update LOOP.md with the exact command, and verify it does not create another PR.
2. Gate Worker: P0-003 / N1 extraction-completeness gate. Abort review when extraction sections are empty, body density is too low, or theorem environments expected from source are missing.
3. Gate Worker: N2 explicit specialist-failure artifacts: every specialist timeout/failure emits a failed or partial artifact with status and reason.
4. Gate Worker: N3 gate input completeness: policy gate and meta recommendation require all upstream artifacts present, schema-valid, and extraction-completeness green.
5. Gate Worker: N4 bundle completeness: every declared artifact exists or has an honest `skip_reason`.
6. Gate Worker / Verifier Worker: N5 false-proof halt: Lean `PROVED` on Tier C/G flawed claims halts all workers and writes an escalation dossier.
7. Citation Worker: P0-004 deterministic resolver waterfall, cache, partial results, chunked timeouts, per-reference statuses, retraction screen, Gemini-grounded fallback with URL evidence and quorum.
8. Coordinator / Verifier Worker: P0-005 PR fixer timeout, deferred until P0-002/P0-003 stop invalid PR-fixer execution.
9. Corpus Auditor / Gate Worker: Tier E/F/G synthetic papers: author and enable fake-citation, prompt-injection, and false-theorem entries.
10. Coordinator / Verifier Worker: toolchain and corpus pins: pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.

## Completed Queue Items

- P0-001 F3 stale runtime binary: fixed locally by reinstalling `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run and real run reached the review-loop path.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
