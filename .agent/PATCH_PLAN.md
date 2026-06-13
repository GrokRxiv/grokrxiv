# GrokRxiv Local Harness Patch Plan

P0 audit has raw evidence for the first regression entry. Work this queue top-down, one defect per local session or worker branch.

## Seeded Queue

1. Extraction Worker: P0-006 source-to-body full extraction recovery for `regression-pr54-weyl`. Diagnose why `pandoc_tex_to_markdown`/source conversion produced 0-byte `body.md`; recover full body with theorem environments or fail extraction before persistence.
2. Gate Worker: N2 explicit specialist-failure artifacts: every specialist timeout/failure emits a failed or partial artifact with status and reason.
3. Gate Worker: N3 gate input completeness: policy gate and meta recommendation require all upstream artifacts present, schema-valid, and extraction-completeness green.
4. Gate Worker: N4 bundle completeness: every declared artifact exists or has an honest `skip_reason`.
5. Gate Worker / Verifier Worker: N5 false-proof halt: Lean `PROVED` on Tier C/G flawed claims halts all workers and writes an escalation dossier.
6. Citation Worker: P0-004 deterministic resolver waterfall, cache, partial results, chunked timeouts, per-reference statuses, retraction screen, Gemini-grounded fallback with URL evidence and quorum.
7. Coordinator / Verifier Worker: P0-005 PR fixer timeout, deferred until P0-002/P0-003 stop invalid PR-fixer execution.
8. Corpus Auditor / Gate Worker: Tier E/F/G synthetic papers: author and enable fake-citation, prompt-injection, and false-theorem entries.
9. Coordinator / Verifier Worker: toolchain and corpus pins: pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.

## Completed Queue Items

- P0-001 F3 stale runtime binary: fixed locally by reinstalling `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run and real run reached the review-loop path.
- P0-002 no-publishing guardrail: fixed locally with `--no-external-actions`, app catalog/help coverage, app-runtime parser/dispatch tests, PATH binary install, and safe dry-run evidence. No full corpus rerun yet.
- P0-003 N1 review-on-empty-body guard: fixed locally with app-runtime extraction-completeness gate before review row creation/specialist launch. Affected regression entry now fails at Extract instead of producing a verdict or PR.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
