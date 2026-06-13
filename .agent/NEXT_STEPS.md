# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 9: continue local-only P0 from the P0-009 checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-009: N3 gate input completeness is fixed locally for required specialist artifacts. Live review DAG gating and persisted publication-gate reconstruction now evaluate outputs against DAG-declared `feeds_meta` roles, so missing required roles are blocked and cannot shrink `expected_total`.

P0-009 validation:
- New fixture first failed before implementation because `SpecialistGate::evaluate_required_roles` did not exist.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_gate_blocks_meta_when_required_roles_are_missing -- --nocapture`: pass after fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime gate -- --nocapture`: pass, 12 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_failure -- --nocapture`: pass, 3 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture`: pass, 264 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.

Residual: no full affected `regression-pr54-weyl` review-loop rerun was executed after P0-009 because it invokes the full multi-agent loop. Tier R is not green until a safe review-loop run verifies all specialists complete, citation partial results exist, and citation `needs_review <= 2`.

Next queue item: N4 bundle completeness. Every declared artifact must exist or have an honest `skip_reason`; the loop should never treat an incomplete artifact bundle as complete. Start with a red fixture before production code.

Known unrelated blocker from P0-006/P0-007 smokes:
- Fresh extraction materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

N3 residual to keep in mind:
- P0-003 enforces extraction completeness before review row creation, but extraction-completeness is not yet persisted as a first-class policy-gate input. Do not claim full N3 corpus coverage until a safe review-loop or persisted policy-path check proves this path.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
