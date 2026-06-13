# GrokRxiv Local Harness Patch Plan

P0 audit has raw evidence for the first regression entry. Work this queue top-down, one defect per local session or worker branch.

## Seeded Queue

1. Gate Worker: N3 gate input completeness: policy gate and meta recommendation require all upstream artifacts present, schema-valid, and extraction-completeness green.
2. Gate Worker: N4 bundle completeness: every declared artifact exists or has an honest `skip_reason`.
3. Gate Worker / Verifier Worker: N5 false-proof halt: Lean `PROVED` on Tier C/G flawed claims halts all workers and writes an escalation dossier.
4. Citation Worker: P0-004 deterministic resolver waterfall, cache, partial results, chunked timeouts, per-reference statuses, retraction screen, Gemini-grounded fallback with URL evidence and quorum.
5. Coordinator / Verifier Worker: P0-005 PR fixer timeout, deferred until P0-002/P0-003 stop invalid PR-fixer execution.
6. Corpus Auditor / Gate Worker: Tier E/F/G synthetic papers: author and enable fake-citation, prompt-injection, and false-theorem entries.
7. Coordinator / Verifier Worker: toolchain and corpus pins: pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.

## Completed Queue Items

- P0-001 F3 stale runtime binary: fixed locally by reinstalling `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run and real run reached the review-loop path.
- P0-002 no-publishing guardrail: fixed locally with `--no-external-actions`, app catalog/help coverage, app-runtime parser/dispatch tests, PATH binary install, and safe dry-run evidence. No full corpus rerun yet.
- P0-003 N1 review-on-empty-body guard: fixed locally with app-runtime extraction-completeness gate before review row creation/specialist launch. Affected regression entry now fails at Extract instead of producing a verdict or PR.
- P0-006 source-to-body empty-body recovery: fixed locally by failing closed on empty TeX conversion, marking empty `body.md` source-to-body stages failed, and treating failed extraction stages as audit failures. No-cache, no-VLM extraction for `2606.00799` now regenerates a 50,697-byte body and 5 sections via PDF fallback. Residual theorem/equation recovery is queued as P0-007.
- P0-007 theorem/equation recovery: fixed locally by adding raw TeX fallback after converter failure, canonicalizing theorem aliases, detecting `construction` theorem-like blocks, and reporting honest `raw_tex_markdown_fallback` provenance. No-cache, no-VLM extraction for `2606.00799` now materializes `body.md` 117,247 bytes, 903 equations, and 41 theorem nodes.
- P0-008 N2 explicit specialist-failure artifacts: fixed locally by preserving schema-valid fallback role outputs while forcing synthetic runner-failure rows to `verifier_status=fail` and adding `verifier_notes.agent_execution={status,role,reason}` to the rendered artifact envelope. Targeted tests and app workspace check passed; full affected review-loop rerun remains pending.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
