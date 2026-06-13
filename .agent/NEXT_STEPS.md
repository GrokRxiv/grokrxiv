# GrokRxiv Local Harness Next Steps

Continue exactly from here:

```text
Phase 0, session 16: continue local-only P0 from the P0-015 local-Gemini-grounded-resolver checkpoint. Do not use Codex Cloud, cloud apply, or cloud task state.

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
- P0-004a: PR-54 classics citation waterfall is fixed locally. Plain references now use Crossref first, then OpenAlex, Semantic Scholar, NASA ADS, INSPIRE-HEP, and zbMATH with bounded per-provider lookups, title normalization/transliteration, cached final status, and per-entry `verified_via` evidence.
- P0-004b: retraction screening is fixed locally. DOI Crossref lookups now parse production retraction metadata (`update-to`, `updated-by`, relation retraction markers, `RETRACTED:` titles), mark such entries `status="retracted"` with `source="crossref_retraction"`, fail the citation gate, preserve `crossref_retraction` through citation-validation reports as remediation evidence, and surface `retracted=<n>` in CLI citation summaries.
- P0-004c: grounded fallback/provider headers are fixed locally. Plain-reference resolver residue can now flow to a config-gated final `gemini_grounded` provider after Crossref/OpenAlex/Semantic Scholar/ADS/INSPIRE/zbMATH. Grounded hits require matching title plus HTTP URL evidence, and Semantic Scholar/ADS requests send local env auth headers when present.
- P0-004d: local Gemini grounded API fallback is fixed locally. When `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL` is unset and a Gemini API key is present, the verifier can call Gemini `generateContent` directly with Google Search grounding, parse JSON output plus grounding metadata URLs, and still require matching-title plus HTTP URL evidence before resolving residue.

P0-013 validation:
- New verifier fixture first failed before implementation because the retracted DOI was treated as `status=resolved` and verifier `Pass`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier doi_crossref_retraction_metadata_marks_gate_failed -- --nocapture`: pass, 1 test.
- New CLI-summary fixture first failed because `CitationVerifierSummary` had no `retracted` field.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation_verifier_summary_surfaces_retracted_entries -- --nocapture`: pass, 1 test.
- The app-runtime citation run includes `citation_validation_report_preserves_retraction_evidence`, proving report/schema preservation of `crossref_retraction`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 31 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.

P0-014 validation:
- New grounded fallback fixture first failed before implementation because `CitationVerifier::with_bibliographic_and_grounded_provider_bases` did not exist.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier grounded_fallback_resolves_residue_with_url_evidence -- --nocapture`: pass, 1 test.
- New provider-header fixture first failed because Semantic Scholar and ADS requests did not include the expected auth headers and therefore hit mock 404s.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier provider_requests_include_semantic_scholar_and_ads_auth_headers -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 33 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.

P0-015 validation:
- New local Gemini API fixture first failed before implementation because `CitationVerifier::with_bibliographic_and_local_gemini_grounded_provider_bases` did not exist.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier local_gemini_grounded_api_resolves_residue_with_grounding_metadata -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier grounded -- --nocapture`: pass, 2 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier default_providers_include_local_gemini_api_when_key_is_configured -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 35 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.

Parallel-test note:
- Full parallel app-runtime lib runs are currently flaky in config/env-heavy tests. In P0-010, two parallel runs failed on different tests (`supervisor::tests::apply_revisions_errors_without_db`, then `state::tests::build_agent_registry_applies_resolved_model_override`), while both tests passed individually and the full suite passed serially. Treat this as residual test-isolation debt, not a P0-010 regression.

Residual: the full affected review-loop rerun after P0-015 was executed safely and did not invoke external actions, but Tier R is still not green. Citation partial results are non-empty, but residue is `unverified=5` against expected `<= 2`; Haskell/Lean/semantic adequacy, PR fixer, and policy gate also remain red.

Next queue item: P0-018 runtime stall before retrying P0-004. The post-P0-004e affected rerun was attempted after reinstalling PATH binaries from `39b9a64`, but `20260613T025743Z` produced a zero-byte log for 12.5 minutes and parked with only local DB sockets. Before another full affected rerun, check no duplicate runs exist, verify local DB responsiveness, and consider running the app binary directly with the same args to isolate `agh`/adapter buffering from app-runtime launch.

P0-004 live citation reliability remains open. The latest completed safe affected run was 2026-06-13T02:30Z review `83675683-633c-44a4-b9c6-0569eee2ddeb`; it proved citation artifacts are partial/non-empty but still had `unverified=5` (`Cartan`, `Ehlers`, `March`, `Reichenbach`, `Trautman`) against Tier R expected `<= 2`. The structured-title bibliographic query fix is tested but not yet proven by a completed affected rerun. Repo `.env` plus split env files currently lack `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL`, `GOOGLE_GENERATIVE_AI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `SEMANTIC_SCHOLAR_API_KEY`, `NASA_ADS_API_TOKEN`, and `ADS_API_TOKEN`; configure a real local grounded resolver endpoint, Gemini API key, ADS token, or add another deterministic provider if the structured-title rerun still leaves residue above target.

cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked
cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked
agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions

If citation `needs_review`/`unverified` is still above 2 or citation artifacts are empty, write a new P0-004 dossier instead of tuning timeouts blindly.

Known red stages from the latest safe run:
- Haskell typed-IR contract: `SemanticModel.hs must define typed mathematical IR type MathType`; keep under P2 typed-IR unless P0 explicitly changes what the Tier R gate considers.
- PR fixer timeout: P0-005 is now confirmed on valid inputs and should be worked after P0-004 is green or formally blocked.
- Policy gate: current code requires `accept`; add a fixture before changing behavior because Tier R only requires `recommendation: honest`.

Known unrelated blocker from P0-006/P0-007 smokes:
- Fresh extraction materializes local artifacts, then exits 1 because the configured data-repo remote `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.

Do not run approve, request-revisions, publisher, close, withdraw, or merge actions from the corpus loop.
Do not run no-cache extraction without `GROKRXIV_INGEST_SKIP_STAGES=vlm` unless you intend to invoke the configured PDF/VLM extraction agent.
After the next fix, update .agent files, append LEDGER.md, run git status, and checkpoint commit.
```
