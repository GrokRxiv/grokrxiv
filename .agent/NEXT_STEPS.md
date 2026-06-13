# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 11: continue local-only P0 from the P0-011 checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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

Fixed P0 items so far:
- P0-002: `--no-external-actions` prevents corpus-loop PR/publisher side effects.
- P0-003: N1 extraction-completeness gate aborts before review row creation/specialists/PR when extracted body is empty or too small.
- P0-006: source-to-body false-success path fails closed on empty TeX conversion and extraction audit treats failed stages as failures.
- P0-007: raw TeX fallback recovers a reviewable body, theorem aliases, construction blocks, equations, and reports `raw_tex_markdown_fallback`.
- P0-008: N2 explicit specialist-failure artifacts are fixed locally. Specialist runner errors/join failures persist schema-valid fallback outputs, force `review_agents.verifier_status=fail`, and render `verifier.notes.agent_execution={status:"failed", role, reason}`.
- P0-009: N3 gate input completeness is fixed locally for required specialist artifacts. Live review DAG gating and persisted publication-gate reconstruction now evaluate outputs against DAG-declared `feeds_meta` roles.
- P0-010: N4 bundle completeness is fixed locally. Review-loop runs write `review_loop/bundle_completeness.json`, policy blocks on manifest-declared non-terminal artifacts missing without `skip_reason`, citation adjudication has an explicit skip artifact until the real DAG output is wired, and PR attachments are derived from `review-loop.yaml` outputs plus harness sidecars.
- P0-011: N5 false-proof halt is fixed locally. Review-loop runs match persisted review sources to `evals/corpus.yaml`, halt Tier C/G Lean `PROVED` results before downstream citation/PR-fix work, write `review_loop/never_event_dossier.json`, mark policy/report/publish-decision artifacts halted, and suppress PR side effects for halted loop outcomes.

P0-011 validation:
- New N5 fixture first failed before implementation because `ReviewLoopCorpusContext` and `review_loop_n5_false_proof_halt` did not exist.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_ -- --nocapture`: pass, 12 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 272 tests.

Parallel-test note:
- Full parallel app-runtime lib runs are currently flaky in config/env-heavy tests. In P0-010, two parallel runs failed on different tests (`supervisor::tests::apply_revisions_errors_without_db`, then `state::tests::build_agent_registry_applies_resolved_model_override`), while both tests passed individually and the full suite passed serially. Treat this as residual test-isolation debt, not a P0-010 regression.

Residual: no full affected review-loop rerun was executed after P0-011 because Tier G synthetic source is still `to_author` and Tier C full review-loop execution invokes the full multi-agent path. Tier R is not green until a safe review-loop run verifies full extraction, all specialists, bundle completeness, citation partial results, and citation `needs_review <= 2`.

Next queue item: P0-004 citation reliability. Implement the deterministic resolver waterfall/cache/partial-result contract for PR-54 classics: Crossref -> OpenAlex -> Semantic Scholar -> NASA ADS -> INSPIRE-HEP -> zbMATH Open, title normalization/transliteration, chunked fan-out with per-reference timeout/status, retraction screening, and Gemini grounded fallback only for unresolved residue with URL evidence/quorum. Start with a red fixture for the Weyl classics expectation (`needs_review <= 2`) and partial-result emission; do not tune timeouts blindly.

Known unrelated blocker from P0-006/P0-007 smokes:
- Fresh extraction materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
