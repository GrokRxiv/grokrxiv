# Ingest and Extraction Gap Analysis

Scope: forced re-extraction and artifact audit for `2605.19178` after the
TeX, citation, source-manifest, and DAG-tool changes.

Command:

```bash
GROKRXIV_INGEST_NO_CACHE=1 GROKRXIV_DRY_RUN_STORAGE=1 \
  cargo run -p grokrxiv-orchestrator --features full --bin grokrxiv -- \
  extract 2605.19178 --json
```

Result: PASS. The CLI artifact audit returned `review_ready=true` with no
warnings or failures.

Observed artifact counts:

- `body.md`: 107222 chars
- sections: 14
- equations: 694
- citations: 146
- citation contexts: 74
- theorem nodes: 0
- extraction stages: 8
- source archive entries: 30
- source figures inventoried: 27
- unmatched citation keys: 0
- uncited bibliography keys: 102

Closed gaps:

- The source archive is inventoried instead of only unpacking top-level TeX.
- `source_manifest.json` records every safe archive entry with size, sha256,
  and kind.
- Figures under `figs/` are detected as source entries and routed as figures
  when object storage is enabled.
- `body.md` now preserves Pandoc citation markers, and `references.json`
  carries citation contexts, cited/uncited status, unmatched keys, and raw
  bibliography metadata.
- BibTeX keys containing DOI punctuation and colons, including
  `Barra:2012aa`, resolve back to the correct bibliography entries.
- `extraction_report.json` records stage/tool/artifact provenance and validates
  against the data repo schema.
- `paper-extract.yaml` exposes tool nodes and the downstream `paper-review`
  DAG call as manifest data.

Remaining gaps:

- `validation` is still `null` in the default `pandoc_enabled` path. LLM-backed
  citation validation is wired into the citation agent schema/prompt, but it
  only runs when extraction agent mode is enabled.
- LaTeXML semantic AST extraction is opt-in. The audit run skipped `tex_to_ast`
  because `GROKRXIV_TEX_ENABLE_LATEXML` was not set.
- Figure binaries are uploaded only when Tier-2 storage is enabled. In
  `GROKRXIV_DRY_RUN_STORAGE=1`, they are inventoried in `source_manifest.json`
  but not materialized under `papers/<arxiv_id>/figures/`.
- Theorem graph extraction is still a markdown/AST scanner. This paper did not
  produce theorem nodes, and the audit did not flag that as a failure because
  the body has no theorem-like signal.
