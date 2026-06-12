# GrokRxiv Local Harness Patch Plan

P0 session 1 is audit-only. Do not patch until findings are recorded with raw evidence.

## Seeded Queue

1. Gate Worker: N1 extraction-completeness gate: abort review when extraction sections are empty, body density is too low, or theorem environments expected from source are missing.
2. Gate Worker: N2 explicit specialist-failure artifacts: every specialist timeout/failure emits a failed or partial artifact with status and reason.
3. Gate Worker: N3 gate input completeness: policy gate and meta recommendation require all upstream artifacts present, schema-valid, and extraction-completeness green.
4. Gate Worker: N4 bundle completeness: every declared artifact exists or has an honest `skip_reason`.
5. Gate Worker / Verifier Worker: N5 false-proof halt: Lean `PROVED` on Tier C/G flawed claims halts all workers and writes an escalation dossier.
6. Citation Worker: deterministic resolver waterfall, cache, partial results, chunked timeouts, per-reference statuses, retraction screen, Gemini-grounded fallback with URL evidence and quorum.
7. Corpus Auditor / Gate Worker: Tier E/F/G synthetic papers: author and enable fake-citation, prompt-injection, and false-theorem entries.
8. Coordinator / Verifier Worker: toolchain and corpus pins: pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
