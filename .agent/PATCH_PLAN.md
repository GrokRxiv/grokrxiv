# GrokRxiv Local Harness Patch Plan

P0 audit has raw evidence for the first regression entry. Work this queue top-down, one defect per local session or worker branch.

## Seeded Queue

1. Extraction Worker: P0-007 theorem/equation recovery for `regression-pr54-weyl`. P0-006 recovered a nonempty PDF fallback body, but `equations.json` and `theorem_graph.json` are still empty; recover theorem/equation artifacts from TeX/PDF or persist honest skipped/failed extraction reasons before claiming Tier R green.
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
- P0-006 source-to-body empty-body recovery: fixed locally by failing closed on empty TeX conversion, marking empty `body.md` source-to-body stages failed, and treating failed extraction stages as audit failures. No-cache, no-VLM extraction for `2606.00799` now regenerates a 50,697-byte body and 5 sections via PDF fallback. Residual theorem/equation recovery is queued as P0-007.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
