# GrokRxiv Local Harness Patch Plan

P0 audit has raw evidence for the first regression entry. Work this queue top-down, one defect per local session or worker branch.

## Seeded Queue

1. Citation Worker: P0-004 residual reliability: add retraction screen, Gemini-grounded fallback with URL evidence/quorum, provider auth/header handling if local env needs it, and affected Tier R rerun proving citation `needs_review <= 2` without wholesale-empty artifacts.
2. Coordinator / Verifier Worker: P0-005 PR fixer timeout, deferred until P0-002/P0-003 stop invalid PR-fixer execution and P0-004 proves the citation stage is no longer the first Tier R blocker.
3. Corpus Auditor / Gate Worker: Tier E/F/G synthetic papers: author and enable fake-citation, prompt-injection, and false-theorem entries.
4. Coordinator / Verifier Worker: toolchain and corpus pins: pin `lake`, Lean/mathlib, `ghc`, and all `pin_on_first_run` arXiv versions.

## Completed Queue Items

- P0-001 F3 stale runtime binary: fixed locally by reinstalling `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run and real run reached the review-loop path.
- P0-002 no-publishing guardrail: fixed locally with `--no-external-actions`, app catalog/help coverage, app-runtime parser/dispatch tests, PATH binary install, and safe dry-run evidence. No full corpus rerun yet.
- P0-003 N1 review-on-empty-body guard: fixed locally with app-runtime extraction-completeness gate before review row creation/specialist launch. Affected regression entry now fails at Extract instead of producing a verdict or PR.
- P0-006 source-to-body empty-body recovery: fixed locally by failing closed on empty TeX conversion, marking empty `body.md` source-to-body stages failed, and treating failed extraction stages as audit failures. No-cache, no-VLM extraction for `2606.00799` now regenerates a 50,697-byte body and 5 sections via PDF fallback. Residual theorem/equation recovery is queued as P0-007.
- P0-007 theorem/equation recovery: fixed locally by adding raw TeX fallback after converter failure, canonicalizing theorem aliases, detecting `construction` theorem-like blocks, and reporting honest `raw_tex_markdown_fallback` provenance. No-cache, no-VLM extraction for `2606.00799` now materializes `body.md` 117,247 bytes, 903 equations, and 41 theorem nodes.
- P0-008 N2 explicit specialist-failure artifacts: fixed locally by preserving schema-valid fallback role outputs while forcing synthetic runner-failure rows to `verifier_status=fail` and adding `verifier_notes.agent_execution={status,role,reason}` to the rendered artifact envelope. Targeted tests and app workspace check passed; full affected review-loop rerun remains pending.
- P0-009 N3 gate input completeness: fixed locally by evaluating live and persisted specialist gates against DAG-declared required specialist roles. Missing required roles now block `meta_can_run` and publication instead of shrinking `expected_total`. Targeted gate tests, full app-runtime lib tests, and app workspace check passed; full affected review-loop rerun remains pending.
- P0-010 N4 bundle completeness: fixed locally by writing `review_loop/bundle_completeness.json`, gating policy on manifest-declared non-terminal artifact outputs missing without `skip_reason`, materializing an explicit citation-adjudication skip artifact, and deriving PR attachments from `review-loop.yaml` outputs plus harness sidecars. Targeted N4 tests, serial full app-runtime lib tests, and app workspace check passed; full affected review-loop rerun remains pending.
- P0-011 N5 false-proof halt: fixed locally by matching review-loop runs to `evals/corpus.yaml` context, halting Tier C/G Lean `PROVED` results before downstream work, writing `never_event_dossier.json`, marking policy/report/publish-decision artifacts halted, and suppressing PR side effects for halted loop outcomes. Targeted review-loop tests, serial full app-runtime lib tests, and app workspace check passed; full affected review-loop rerun remains pending.
- P0-004a PR-54 classics citation waterfall: fixed locally by adding a deterministic Crossref -> OpenAlex -> Semantic Scholar -> NASA ADS -> INSPIRE-HEP -> zbMATH resolver path for plain references, per-provider timeout/status handling, final per-reference caching, title normalization/transliteration, and `verified_via` evidence. The hermetic classics fixture resolves four of six refs through ADS/zbMATH and leaves exactly two unverified residues. Retraction/Gemini/full affected rerun remain queued under P0-004 residual reliability.

## Work Rule

After P0 audit, each implementation session takes exactly one defect:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus rerun -> checkpoint commit
```

Coordinator assigns one queue item at a time into a local worktree under `.agent/worktrees/`. Workers must not merge themselves back to the integration branch.
