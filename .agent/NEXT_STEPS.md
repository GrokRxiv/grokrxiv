# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 5: fix P0-006 / source-to-body full extraction recovery for regression-pr54-weyl. Use local Codex only; do not use Codex Cloud, cloud apply, or cloud task state.

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

P0-003 is fixed locally for the N1 never-event: the review now aborts before review row creation/specialists/PR when extracted body sections are empty or body text is below 1,000 chars. The affected regression entry still fails because extraction itself is empty.

Review the P0-006 dossier:
- .agent/FINDINGS.md
- agenthero/apps/grokrxiv/evals/results/20260613T000936Z/regression-pr54-weyl/run.log
- /Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md
- /Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/sections.json
- /Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/source_manifest.json
- /Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/extraction_report.json

Start P0-006 with a failing fixture test at the source-to-body/pandoc conversion layer:
- reproduce a source conversion that returns 0-byte `body.md` while reporting `source_to_body` ok
- expected behavior: recover a full Markdown body with sections/theorem-like text when source is usable, or persist an explicit failed extraction stage before reviewability is claimed
- do not mask this by raising timeouts or rerunning full review

Expected P0-006 implementation shape:
- diagnose `source_manifest.json` and the cached source availability for arXiv:2606.00799
- patch the app-owned ingest/extraction code under `agenthero/apps/grokrxiv/`
- keep the P0-003 extraction-completeness gate intact
- rerun the narrow extraction/review command with `--no-external-actions`; the next success criterion is nonempty body/sections or a correctly failed extraction report, not a downstream review verdict

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
After P0-006, continue top-down in .agent/PATCH_PLAN.md. Full sweeps only when the plan says the phase might be done.

Append agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Update .agent/AGENT_STATUS.md and .agent/TEST_LOG.md.
End with git status and a checkpoint commit.
```
