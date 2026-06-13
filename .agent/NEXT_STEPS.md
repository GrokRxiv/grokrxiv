# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 7: continue local-only P0 from the P0-007 checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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

P0-007 is fixed locally for theorem/equation recovery:
- Raw TeX fallback recovers a reviewable body after converter failure.
- Theorem aliases from `\newtheorem` are canonicalized for deterministic scanners.
- `construction` theorem-like blocks are detected and label-resolved.
- `source_to_body` provenance reports `raw_tex_markdown_fallback`.
- No-cache, no-VLM extraction for `2606.00799` materialized local artifacts with `body.md` 117,247 bytes, 903 equations, and 41 theorem nodes.

Residual: Tier R is not green until a safe review-loop run verifies all specialists complete, citation partial results exist, and citation `needs_review <= 2`.

Next queue item: N2 explicit specialist-failure artifacts. Every specialist timeout/failure must emit a failed or partial artifact with status and reason before meta/policy can treat the run as complete. Work this unless a coordinator chooses to run a safe affected review first.

Known unrelated blocker from P0-006/P0-007 smokes:
- Fresh extraction materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
