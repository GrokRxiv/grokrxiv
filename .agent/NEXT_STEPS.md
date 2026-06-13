# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 8: continue local-only P0 from the P0-008 checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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

P0-008 validation:
- New fixture first failed before implementation because `specialist_failure_verifier_result` did not exist.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_failure -- --nocapture`: pass.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime gate -- --nocapture`: pass.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture`: pass, 263 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.

Residual: no full affected `regression-pr54-weyl` review-loop rerun was executed after P0-008 because it invokes the full multi-agent loop rather than cheaply isolating the N2 failure path. Tier R is not green until a safe review-loop run verifies all specialists complete, citation partial results exist, and citation `needs_review <= 2`.

Next queue item: N3 gate input completeness. Policy gate and meta recommendation must never compute from incomplete upstream inputs; gate requires presence + schema-validity of every required upstream artifact including the extraction-completeness flag. Work this unless the coordinator chooses to run the safe affected review-loop first.

Known unrelated blocker from P0-006/P0-007 smokes:
- Fresh extraction materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
