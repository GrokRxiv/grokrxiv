# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 18: continue local-only P0 from the P0-004f citation-green checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

Read:
- agenthero/apps/grokrxiv/evals/corpus.yaml
- agenthero/apps/grokrxiv/evals/LOOP.md
- agenthero/apps/grokrxiv/evals/PHASES.md
- .agent/AGENT_STATUS.md
- .agent/FINDINGS.md
- .agent/PATCH_PLAN.md
- .agent/TEST_LOG.md
- agenthero/apps/grokrxiv/evals/results/LEDGER.md

Corpus review runs must use:

agh --json app run grokrxiv review <source> --loop --debug --no-external-actions

Current state:
- P0-004 citation reliability is green for Tier R on local CLI.
- Latest affected run: 20260613T045516Z, review `3619ff6a-1a72-4aa0-bb0f-c8bbcacd8cc3`, product exit 0, `external_actions_enabled=false`, `pr_url=null`.
- Citation report: `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`. Remaining residues are both March references and are within the Tier R `<= 2` threshold.
- No full corpus-green claim and no phase tag.

Next queue item: P0-020 review-loop math-source artifact loss.
- Persisted extraction cache for `2606.00799` is healthy: `body.md` 117,247 bytes, `sections.json` 8 sections, `equations.json` 903 entries, `theorem_graph.json` 41 nodes.
- Latest review-loop artifact dropped that signal: `review_loop/paper_math_sources.json` recorded zero theorem nodes and only three equations; stderr summarized `paper_math_source_collector [OK] ... theorem_nodes=0 equations=0 sources=1`.
- Add a failing fixture for `paper_math_source_collector` loading persisted `equations.json` and `theorem_graph.json`, then fix the collector/input path wiring.
- Re-run the affected Tier R entry and require `paper_math_sources.json` to preserve non-empty theorem/equation artifacts.

Known red stages after P0-004f:
- Haskell typed-IR/semantic validation fails. Keep deterministic typed-IR/Lean emission under P2 unless P0 explicitly narrows this gate.
- PR fixer times out after 360s on valid inputs; P0-005 is next after P0-020.
- Policy gate requires `accept`; add a fixture for Tier R `expected.recommendation: honest` before changing behavior.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not weaken `expected:` blocks or NEVER-events.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
