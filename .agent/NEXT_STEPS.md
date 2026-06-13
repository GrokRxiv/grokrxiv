# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 6: continue local-only P0 from the P0-006 checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

P0-002 is fixed locally. Corpus review runs must use:

agh --json app run grokrxiv review <source> --loop --debug --no-external-actions

P0-003 is fixed locally for N1 review-on-empty-body: review aborts before review row creation/specialists/PR when extracted body sections are empty or body text is below 1,000 chars.

P0-006 is fixed locally for source-to-body empty-body false success:
- TeX bundle parsing fails closed when Pandoc/LaTeXML produce no Markdown.
- `source_to_body` reports `failed` when `body.md` is empty.
- extraction audit treats failed stages as failures.
- No-cache, no-VLM extraction for `2606.00799` regenerated local artifacts with `body.md` 50,697 bytes and 5 sections via PDF fallback.

Residual: `equations.json` and `theorem_graph.json` are still empty for `regression-pr54-weyl`; Tier R is not green. Work the new top PATCH_PLAN item P0-007 unless a coordinator decides to rerun the affected review first:
- recover theorem/equation artifacts from TeX/PDF, or
- persist honest skipped/failed extraction reasons for those stages before claiming Tier R green.

Known unrelated blocker from P0-006 smoke:
- `GROKRXIV_INGEST_NO_CACHE=1 GROKRXIV_INGEST_SKIP_STAGES=vlm cargo run --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --bin grokrxiv-app -- --json extract 2606.00799` materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
