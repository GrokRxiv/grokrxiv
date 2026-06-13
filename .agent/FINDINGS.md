# GrokRxiv Local Harness Findings

## P0-001: Product Review Loop Cannot Start From PATH Runtime

ID: P0-001
Corpus entry: `regression-pr54-weyl`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 1
finish_reason: adapter/runtime argument parse failure
Bucket: F3 toolchain
NEVER-event: none reached; review did not start
Symptom: PATH `agh` reaches the GrokRxiv app adapter, but the installed `/Users/mlong/.cargo/bin/grokrxiv-app` rejects `--loop` before any corpus artifact is produced.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-url.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/runtime-installed-dry-run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/runtime-source-url-dry-run.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/provenance.json`
Artifact paths: none; review did not start.
Root cause: installed runtime binary is stale. The manifest declares `--loop` and current source parses it, but PATH `grokrxiv-app` predates that parser.
Owning code/surface:
- `/Users/mlong/.cargo/bin/grokrxiv-app`
- `agenthero/apps/grokrxiv/rust/src/main.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
- `agenthero/apps/grokrxiv/app.yaml`
Fix plan:
1. Install current app runtime binary: `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`.
2. Install current app adapter binary: `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`.
3. Re-run `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`.
4. If it still fails before review start, add product-surface coverage that executes the adapter/runtime path, then fix adapter runtime resolution.
Attempts: 1
Escalation status: none.

## P0-001 Resolution

Status: fixed locally, 2026-06-12T23:27Z.
Evidence:
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass.
- `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`: pass.
- `agh --dry-run app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`: pass, emitted review-loop stage plan.
- Real product run then started and completed as review `eca527eb-3930-49e6-a828-66dd64611430`.

## P0-002: Corpus Loop Opened A PR Despite No-Publishing Guardrail

ID: P0-002
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: none declared yet; violates `LOOP.md` hard guardrail 5.
Symptom: the corpus command opened `https://github.com/GrokRxiv/grokrxiv-reviews/pull/55` with `pr_kind=revision_needed` and `status=pr_open`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-after-install.log`
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-002-no-pr-guardrail.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/publish_decision.json`
Root cause: no safe corpus/eval no-external-side-effects mode is wired into the product review-loop command.
Fix plan: add product-surface coverage and a safe local corpus command that disables approve, request-revisions, publisher, and revision PR creation before any further full corpus reruns.
Attempts: 1
Escalation status: PR #55 was opened by the run; do not invoke close/withdraw from the corpus loop without human direction.

## P0-002 Resolution

Status: fixed locally, 2026-06-13T00:00Z.
Evidence:
- Added `--no-external-actions` to `agenthero/apps/grokrxiv/app.yaml` so the app catalog and action help advertise the corpus-safe mode.
- Added app-runtime parser and dispatch coverage for `--no-external-actions`.
- `open_review_pr_after_optional_loop` now runs the optional review loop, evaluates the publication gate, and returns `pr_url: null` without calling publication or revision PR code when external actions are disabled.
- `agenthero/apps/grokrxiv/evals/LOOP.md` now uses `agh --json app run grokrxiv review <source> --loop --debug --no-external-actions`.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib`: pass, 257 tests.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass.
- `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`: pass.
- `agh --json --dry-run app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`: pass, emitted `external_actions.enabled=false` and did not start pipeline work.
Residual:
- No real corpus rerun yet; continue P0-003 first so the next live run fails at extraction completeness instead of proceeding into downstream review/PR-fix stages.

## P0-003: N1 Extraction Completeness Gate Did Not Fire

ID: P0-003
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: N1_review_on_empty_body
Symptom: review proceeded despite empty extraction artifacts: `body.md` is 0 bytes, `sections.json` has 0 sections, `equations.json` has 0 equations, and `theorem_graph.json` has 0 nodes. The extraction report marked these stages `ok`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-003-n1-extraction-gate.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/sections.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/extraction_report.json`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/paper_math_sources.json`
Root cause: ingest/extraction can mark empty body/theorem artifacts successful, and review/policy does not require extraction completeness before downstream verdicts and PR actions.
Fix plan: write failing fixture test for empty body/sections/theorem graph, add extraction-completeness failure artifact, and abort before specialist/meta/policy/PR stages.
Attempts: 1
Escalation status: none.

## P0-003 Resolution

Status: N1 review-on-empty-body guard fixed locally, 2026-06-13T00:10Z.
Evidence:
- Added `extraction_completeness_gate` in `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor/review_flow.rs`.
- The gate rejects empty sections and body text below 1,000 chars before review row creation, specialist launch, meta synthesis, policy, PR fixer, or PR dispatch.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime extraction_completeness_gate`: pass, 2 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib`: pass, 259 tests.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests.
- `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`: exits 1 at `[2/6] Extract [FAIL] extraction completeness failed`.
- Raw affected-entry evidence: `agenthero/apps/grokrxiv/evals/results/20260613T000936Z/regression-pr54-weyl/run.log`.
Residual:
- Tier R is still red against `expected.extraction: full_body_with_theorem_envs`; source-to-body still produced a zero-byte body and `sections.json` is empty. Track as P0-006 before downstream N2/N3 work that depends on a reviewable body.

## P0-006: Source-To-Body Extraction Still Produces Empty Body For Weyl

ID: P0-006
Corpus entry: `regression-pr54-weyl`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 1
finish_reason: extraction-completeness gate blocked review before specialist launch
Bucket: F1 contract
NEVER-event: N1_review_on_empty_body is now blocked, but expected full-body extraction is still failing.
Symptom: the review no longer proceeds on empty body, but the extractor still has `body.md` at 0 bytes, `sections.json` with 0 sections, and no theorem/equation artifacts for the regression paper.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T000936Z/regression-pr54-weyl/run.log`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/sections.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/extraction_report.json`
Root cause: not diagnosed in this session. `source_to_body` / `pandoc_tex_to_markdown` still reports ok despite no body text.
Fix plan: inspect `source_manifest.json`, cached source availability, and `pandoc_tex_to_markdown`; add a fixture where source conversion yields empty output and either recover full body or mark extraction failed before persistence.
Attempts: 1
Escalation status: none.

## P0-006 Resolution

Status: fixed locally for the empty-body false-success path, 2026-06-13T00:28Z.
Root cause:
- `grokrxiv-ingest::tex::parse_bundle` swallowed Pandoc failures by returning empty Markdown and still produced `Ok(TexExtract { sections: [] })`.
- `ingest_pipeline` recorded `source_to_body` as `ok` before it had materialized or validated `body.md`.
Evidence:
- Added a failing fixture for a TeX bundle whose Pandoc conversion exits nonzero; before the fix it returned a successful empty extraction.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest tex::tests -- --nocapture`: pass, 20 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1`: pass, 261 tests.
- `git diff --check`: pass.
- `GROKRXIV_INGEST_NO_CACHE=1 GROKRXIV_INGEST_SKIP_STAGES=vlm cargo run --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --bin grokrxiv-app -- --json extract 2606.00799`: exit 1 after local artifact materialization because configured data-repo push to `git@github.com:GrokRxiv/grokrxiv-data.git` fails with `unsupported URL protocol`.
- Local artifacts regenerated for `2606.00799`: `body.md` is 50,697 bytes, `sections.json` has 5 sections, and `source_to_body` provenance is `pdf_extract_text` with `status: ok`.
Residual:
- The local recovery currently falls back to PDF text, so `equations.json` and `theorem_graph.json` remain empty for the Weyl paper. That is honest and reviewable body recovery, not a full Tier R green result.
- The data-repo push failure is a separate environment/tooling defect and was not fixed here.

## P0-007: Theorem/Equation Artifacts Empty After Source-To-Body Recovery

ID: P0-007
Corpus entry: `regression-pr54-weyl`
Runner: `cli`
Command: `GROKRXIV_INGEST_NO_CACHE=1 GROKRXIV_NO_CACHE=1 GROKRXIV_INGEST_SKIP_STAGES=vlm GROKRXIV_APP_BIN=/nonexistent/grokrxiv-app cargo run -p agenthero-orchestrator --bin agh -- --json app run grokrxiv extract 2606.00799`
Exit code: 1
finish_reason: local extraction materialized artifacts, then Stage 8 failed on configured data-repo remote push (`unsupported URL protocol`)
Bucket: F1 contract
NEVER-event: N1 is blocked by P0-003; Tier R `full_body_with_theorem_envs` was still red before this fix.
Symptom: P0-006 recovered nonempty PDF fallback body, but `equations.json` and `theorem_graph.json` were empty because the source TeX path was discarded after Pandoc failed.
Raw evidence paths:
- `/tmp/grokrxiv-2606.00799.LqWAQu/Weyl-type_theorems.tex`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/equations.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/theorem_graph.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/extraction_report.json`
Root cause:
- Pandoc fails on the Weyl source at bundled `biblatex.sty` (`unexpected :`), causing the old path to discard TeX and fall back to lossy PDF text.
- The raw TeX source contains theorem aliases (`\newtheorem{thm}{Theorem}`, `\newtheorem{constr}[thm]{Construction}`) and many display equations, but the deterministic theorem scanner only recognized canonical environment names.
Owning code:
- `agenthero/apps/grokrxiv/crates/ingest/src/tex.rs`
- `agenthero/apps/grokrxiv/crates/ingest/src/pipeline.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/ingest_pipeline.rs`
- `agenthero/apps/grokrxiv/crates/extraction/src/extraction/theorems/tools.rs`
Fix plan:
1. Add fixture where converter failure must recover a raw-TeX body with canonical theorem and equation environments.
2. Add theorem scanner coverage for `construction` blocks and labels.
3. Thread a source-to-body producer through ingest so extraction reports identify `raw_tex_markdown_fallback` honestly.
4. Re-run affected extraction with VLM skipped and no cache.
Attempts: 1
Escalation status: none.

## P0-007 Resolution

Status: fixed locally for theorem/equation artifact recovery, 2026-06-13T00:49Z.
Evidence:
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest parse_bundle_ -- --nocapture`: pass, 2 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-extraction construction -- --nocapture`: pass, 2 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime source_to_body_report_names_raw_tex_fallback -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime deterministic_equation_fallback_extracts_pandoc_math -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime deterministic_theorem_fallback_extracts_title_headings -- --nocapture`: pass, 1 test.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- Affected extraction command above still exits 1 on data-repo remote push, but local artifacts are materialized: `body.md` 117,247 bytes, `equations.json` 903 entries, `theorem_graph.json` 41 nodes.
- `extraction_report.json` now reports `source_to_body.status=ok`, `source_to_body.tool=raw_tex_markdown_fallback`, `equations.tool=scan_equations`, and `theorems.tool=scan_theorems`.
Residual:
- Tier R is still not full green until the safe review-loop run verifies all specialists, citation partial results, and citation `needs_review <= 2`.
- The data-repo push failure is environment/tooling outside this defect and remains unresolved.

## P0-008: Specialist Runner Failure Could Be Persisted As Schema-Valid Output

ID: P0-008
Corpus entry: `regression-pr54-weyl` / NEVER-event `N2_silent_specialist_loss`
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixture reproduced missing explicit failure marker before implementation
Bucket: F1 contract
NEVER-event: N2_silent_specialist_loss
Symptom: runner failures are caught and converted into role-schema-valid fallback JSON, but the persisted verifier status previously depended on the normal verifier ladder. For non-citation roles, a schema-valid fallback with enough prose can be marked usable by schema/support/tone checks instead of being recorded as a failed specialist artifact with status and reason.
Raw evidence paths:
- `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor/verification.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor/review_flow.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor.rs`
Root cause:
- `specialist_failure_output` intentionally emits closed-schema-valid fallback outputs, but the review-flow result tuple did not remember that the output came from a runner failure.
- `review_agents.verifier_status` and the rendered `agents/<role>.json.verifier` envelope therefore had no guaranteed `fail` status or structured execution-failure reason for those synthetic outputs.
Fix plan:
1. Add a failing fixture asserting specialist execution failures force verifier status `fail` and record `agent_execution.status`, `role`, and `reason` while preserving normal verifier ladder notes.
2. Thread an optional execution-failure reason through `specialist_results`.
3. After the normal verifier ladder runs, override synthetic failure rows to `VerifierStatus::Fail` and merge `agent_execution` notes into `verifier_notes`.
4. Keep the role output JSON schema-valid by storing status/reason in the artifact envelope verifier notes, not inside the closed role output schema.
Attempts: 1
Escalation status: none.

## P0-008 Resolution

Status: fixed locally for explicit specialist-failure artifacts, 2026-06-13T00:59Z.
Evidence:
- Added `specialist_failure_verifier_result` in `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor/verification.rs`.
- `run_review_dag_inner_with_context` now carries `Option<String>` execution-failure reasons for specialist runner errors and join failures.
- Synthetic specialist failure rows now persist `verifier_status=fail` plus `verifier_notes.agent_execution.status=failed`, `role`, and `reason`; rendered `agents/<role>.json` artifacts include the same verifier envelope.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_failure_verifier_result_records_status_role_and_reason -- --nocapture`: expected compile fail before helper, then pass after fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_failure -- --nocapture`: pass, 3 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime gate -- --nocapture`: pass, 11 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture`: pass, 263 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
Residual:
- No full affected `regression-pr54-weyl` review-loop rerun was executed in this checkpoint because it invokes the full multi-agent loop rather than cheaply isolating the N2 failure path.
- Tier R still needs a safe `--no-external-actions` review-loop run to verify `paper_review: all_specialists_complete`, citation partial-result emission, and `needs_review <= 2`.

## P0-004b: Citation Verifier Did Not Screen Crossref Retraction Metadata

ID: P0-004b
Corpus entry: `majorana-quantized` / Tier D retraction expectation; also part of P0 citation reliability.
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixture reproduced Crossref retraction metadata being treated as a normal resolved DOI before implementation
Bucket: F1 contract
NEVER-event: supports Tier D `retraction_detection: flagged_via_external_db`; not a NEVER-event by itself.
Symptom: DOI lookup used only the Crossref HTTP status. A Crossref `/works/{doi}` response with production retraction metadata would be reported as `status=resolved`, `exists=true`, and verifier `Pass`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/crates/verifier/src/citation.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/ingest_pipeline.rs`
- `agenthero/apps/grokrxiv/schemas/citation_validation_report.schema.json`
Root cause:
- `resolve_doi` called a status-only HTTP helper and never parsed Crossref JSON, so it could not inspect `update-to`, `updated-by`, relation metadata, or a `RETRACTED:` title marker.
- CLI citation summaries counted unresolved/unverified/unknown/malformed entries only, so a future `retracted` status would not be surfaced in human-facing review evidence.
Fix plan:
1. Add a failing DOI fixture whose Crossref body has `update-to` and `updated-by` entries with `type: "retraction"`.
2. Parse Crossref JSON in DOI resolution and return `status="retracted"` with explicit evidence when production metadata marks the work retracted.
3. Treat retractions as definitive citation gate failures and prevent arXiv metadata from overriding a retracted DOI.
4. Preserve `crossref_retraction` through the deterministic citation-validation report schema and mark retracted resolver results as remediation items.
5. Add CLI summary coverage for `retracted=<n>` and human evidence.
Attempts: 1
Escalation status: none.

## P0-004b Resolution

Status: fixed locally for Crossref production retraction metadata, 2026-06-13T01:57Z.
Evidence:
- `doi_crossref_retraction_metadata_marks_gate_failed` first failed with verifier `Pass`, entry `status="resolved"`, and `source="crossref"`.
- DOI lookup now parses Crossref JSON and detects `update-to`, `updated-by`, relation keys containing `retract`, and `RETRACTED:` titles.

- Retracted entries produce `status="retracted"`, `exists=false`, `source="crossref_retraction"`, retained DOI/URL, and explicit reason evidence including retraction DOI/source/record id when present.
- The verifier gate fails on any retracted entry and includes a top-level `retracted[]` list in notes.
- Deterministic citation-validation reports preserve `source="crossref_retraction"`, retain evidence strings, and report retracted resolver results as `needs_remediation`.
- CLI citation summaries count `retracted=<n>` and include retraction evidence lines instead of hiding them behind generic needs-review text.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 31 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 275 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- No full affected review-loop rerun was executed in this checkpoint.
- P0-004 residual work remains: Gemini-grounded fallback/quorum for unresolved residue, provider auth/header handling if local env requires it, and the safe Tier R rerun proving citation `needs_review <= 2`.

## P0-004c: Citation Waterfall Had No Grounded Residue Resolver Or Provider Headers

ID: P0-004c
Corpus entry: `regression-pr54-weyl` / Tier R citation `needs_review <= 2` expectation
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixtures reproduced missing grounded fallback API and missing ADS/Semantic Scholar provider headers before implementation
Bucket: F1 contract / F3 provider auth
NEVER-event: supports N2 partial-result robustness and Tier R citation integrity; not a NEVER-event by itself.
Symptom: after Crossref/OpenAlex/Semantic Scholar/ADS/INSPIRE/zbMATH all missed a plain reference, the verifier emitted `source="citation_waterfall"` and `status="unverified"` with no final grounded adjudication path. Provider requests also sent no Semantic Scholar API key or ADS bearer token, so authenticated local resolver endpoints would return misses even when env keys were present.
Raw evidence paths:
- `agenthero/apps/grokrxiv/crates/verifier/src/citation.rs`
- `.env` key presence check: `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL=absent`, `SEMANTIC_SCHOLAR_API_KEY=absent`, `NASA_ADS_API_TOKEN=absent`, `ADS_API_TOKEN=absent`
Root cause:
- `CitationVerifier` had a deterministic provider vector ending at zbMATH and no configured final grounded provider for residue.
- `resolve_provider_bibliographic` used the generic JSON request helper, so provider-specific auth headers could not be attached.
Fix plan:
1. Add a failing fixture for a deterministic waterfall miss that resolves only through a URL-backed grounded response.
2. Add a config-gated `gemini_grounded` bibliographic provider that runs last and requires matching-title plus HTTP URL evidence before returning `resolved`.
3. Add a failing fixture requiring `x-api-key` for Semantic Scholar and `Authorization: Bearer` for ADS.
4. Add provider-specific request headers from `SEMANTIC_SCHOLAR_API_KEY`, `NASA_ADS_API_TOKEN`, or `ADS_API_TOKEN`.
5. Do not claim Tier R green until a real grounded resolver URL is configured and the safe affected review-loop command is rerun.
Attempts: 1
Escalation status: none.

## P0-004c Resolution

Status: fixed locally for the verifier-side grounded fallback contract and provider headers, 2026-06-13T02:11Z.
Evidence:
- `grounded_fallback_resolves_residue_with_url_evidence` first failed before implementation with missing `CitationVerifier::with_bibliographic_and_grounded_provider_bases`; it now passes and shows a waterfall residue resolving via `source="gemini_grounded"` with URL evidence.
- `provider_requests_include_semantic_scholar_and_ads_auth_headers` first failed because Semantic Scholar and ADS mock endpoints returned 404 without expected headers; it now passes with `SEMANTIC_SCHOLAR_API_KEY` and `NASA_ADS_API_TOKEN`.
- Grounded responses are accepted only when verdict/status is `verified`, `resolved`, or `exists`, the returned title matches the citation title, and at least one HTTP URL appears in `evidence_urls`, `urls`, `evidence`, or `quorum` evidence fields.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 33 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- Repo `.env` does not currently configure `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL` or provider keys, so this checkpoint does not prove the live Tier R affected review-loop run.
- The safe affected command remains pending: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`.

## P0-009: Policy Gate Could Infer Completeness From Partial Specialist Rows

ID: P0-009
Corpus entry: `regression-pr54-weyl` / NEVER-event `N3_gate_on_incomplete_inputs`
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixture reproduced missing required-role gate API before implementation
Bucket: F1 contract
NEVER-event: N3_gate_on_incomplete_inputs
Symptom: `load_specialist_gate_for_review` reconstructed the publication gate from rows present in `review_agents` and set `expected_total = statuses.len()`. If only three specialist rows existed and all passed, the policy gate could treat the review as complete enough for meta/policy even though two DAG-declared specialist artifacts were absent.
Raw evidence paths:
- `agenthero/apps/grokrxiv/crates/orchestrator/src/review_gate.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/db.rs`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor/review_flow.rs`
Root cause:
- The gate aggregate accepted an `expected_total` supplied by callers without a required-role list.
- The DB reconstruction path derived the expected total from persisted rows instead of from the `paper-review` DAG's `feeds_meta` specialist roles.
- The live supervisor already had the manifest role list but still passed only statuses plus a scalar total into the aggregate.
Fix plan:
1. Add a failing fixture asserting missing required roles block `meta_can_run` and appear in `blocked_roles`.
2. Add a required-role specialist gate evaluator that expands missing roles to `None`, preserves required role order, and blocks meta/publication when any required role is absent.
3. Use the required-role evaluator in the live review DAG.
4. Use `dag_feeds_meta_roles("paper-review")` in `load_specialist_gate_for_review` so persisted policy checks cannot shrink `expected_total`.
Attempts: 1
Escalation status: none.

## P0-009 Resolution

Status: fixed locally for gate input completeness, 2026-06-13T01:08Z.
Evidence:
- Added `SpecialistGate::evaluate_required_roles` in `agenthero/apps/grokrxiv/crates/orchestrator/src/review_gate.rs`.
- `run_review_dag_inner_with_context` now evaluates specialist verifier statuses against `review_dag.specialist_roles`.
- `load_specialist_gate_for_review` now loads the DAG-declared `paper-review` `feeds_meta` roles and uses them as the expected role set.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_gate_blocks_meta_when_required_roles_are_missing -- --nocapture`: expected compile fail before implementation, then pass after fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime gate -- --nocapture`: pass, 12 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime specialist_failure -- --nocapture`: pass, 3 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture`: pass, 264 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- The extraction-completeness flag is enforced before review row creation by P0-003 but is not yet persisted as a first-class policy-gate input. Track this as residual N3 hardening if a later persisted policy path needs to prove extraction-completeness provenance independently.
- No full affected `regression-pr54-weyl` review-loop rerun was executed in this checkpoint.

## P0-010: Review-Loop Bundles Could Omit Declared Artifacts Without Skip Reasons

ID: P0-010
Corpus entry: `regression-pr54-weyl` / NEVER-event `N4_bundle_missing_declared_artifacts`
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixture reproduced missing bundle-completeness API before implementation
Bucket: F1 contract
NEVER-event: N4_bundle_missing_declared_artifacts
Symptom: `dags/review-loop.yaml` declares artifact outputs such as `citation_validation_adjudication.json` and `review_loop/fixed/review.pdf`, while the runtime could complete with those files absent and only nearby failure state in `citation_validation_report.json` or `pr_fixes.json`. The PR attachment list was hand-maintained, so manifest-declared outputs could drift out of the published/revision bundle.
Raw evidence paths:
- `agenthero/apps/grokrxiv/dags/review-loop.yaml`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
Root cause:
- Review-loop runtime had no manifest-output bundle completeness pass.
- Missing optional/failed outputs were not normalized into per-declared-output statuses with `skip_reason`.
- `append_review_loop_pr_files` used a static file list instead of the manifest output list.
Fix plan:
1. Add a failing fixture for missing declared outputs without skip reasons.
2. Add a fixture proving explicit skip reasons make a missing declared output honest.
3. Emit `review_loop/bundle_completeness.json` before policy over non-terminal manifest outputs.
4. Add policy/report component status for bundle completeness.
5. Make review-loop PR attachments derive from `review-loop.yaml` outputs plus harness sidecars.
Attempts: 1
Escalation status: none.

## P0-010 Resolution

Status: fixed locally for bundle completeness, 2026-06-13T01:21Z.
Evidence:
- Added manifest-output normalization and `review_loop_bundle_completeness_report` in `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`.
- Review-loop runtime now writes `bundle_completeness.json` before policy for non-terminal declared artifact outputs and blocks policy when any declared output is missing without a skip reason.
- The runtime now writes an explicit skipped `citation_validation_adjudication.json` artifact while citation adjudication is not separately materialized.
- Missing Haskell/Lean generated source and PR fixed TeX/PDF outputs get explicit skip reasons derived from the relevant fix-loop result.
- `append_review_loop_pr_files` now derives bundle attachments from `review-loop.yaml` outputs and includes `bundle_completeness.json` plus harness sidecars.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_bundle_completeness_flags_missing_declared_outputs -- --nocapture`: expected compile fail before implementation, then pass after fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_bundle -- --nocapture`: pass, 3 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_stage_plan_is_loaded_from_manifest -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 267 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- Parallel `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture` is flaky in config/env-heavy tests unrelated to P0-010. In this session, `supervisor::tests::apply_revisions_errors_without_db` and then `state::tests::build_agent_registry_applies_resolved_model_override` failed in separate parallel full runs, but each passed individually and the full lib suite passed serially.
- No full affected `regression-pr54-weyl` review-loop rerun was executed in this checkpoint.

## P0-011: N5 False-Proof Halt Was Not Enforced In The Review Loop

ID: P0-011
Corpus entry: `blum-pvnp` / `synthetic-false-theorem`
Runner: `cli`
Command: targeted local tests; no full affected review-loop rerun in this checkpoint
Exit code: targeted validation passed after fix
finish_reason: local TDD fixture reproduced missing N5 detector before implementation
Bucket: F1 contract
NEVER-event: N5_fake_proof
Symptom: the review-loop Lean result path could mark theorem formalization `PROVED` and continue into citation/PR-fix/publish-decision stages without checking whether the run source is a Tier C/G flawed or false corpus entry.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
Root cause:
- `run_review_loop_for_review` built `theorem_map` and `semantic_adequacy` but had no golden-corpus context loader.
- `ReviewLoopOutcome` did not carry halted state into the app-run envelope/meta summary.
- `open_review_pr_after_optional_loop` only honored the operator `--no-external-actions` flag and did not suppress PR side effects when the loop halted on an integrity never-event.
Fix plan:
1. Add a failing fixture asserting Tier C Lean `PROVED` produces an N5 halt dossier.
2. Parse `evals/corpus.yaml` into minimal review-loop corpus context and match persisted review source metadata against corpus `source:` values, including arXiv versions and app-relative synthetic paths.
3. Detect Tier C/G theorem-map `PROVED` immediately after Lean/semantic adequacy and before citation/PR-fixer stages.
4. Write `review_loop/never_event_dossier.json`, halted `policy_gate.json`, halted `review_loop_report.json`, and a non-publishing `publish_decision.json`.
5. Suppress downstream PR side effects for halted loop outcomes even when external actions are enabled.
Attempts: 1
Escalation status: none.

## P0-011 Resolution

Status: fixed locally for N5 false-proof halt, 2026-06-13T01:34Z.
Evidence:
- Added `ReviewLoopCorpusContext` parsing from `agenthero/apps/grokrxiv/evals/corpus.yaml`.
- The corpus matcher handles `https://arxiv.org/abs/<id>vN` against `arxiv:<id>` and app-relative synthetic paths such as `evals/synthetic/false-theorem/`.
- `review_loop_n5_false_proof_halt` emits a structured dossier with `never_event=N5_fake_proof`, `action=halt_and_escalate`, corpus id/tier/source, Lean verdict, proved entries, and artifact pointers.
- `run_review_loop_for_review` now returns early after `semantic_adequacy` when N5 fires, before citation validation, PR fixer, or publishing decisions can continue.
- Halted loop outcomes carry `halted=true` into the result envelope and meta summary.
- `open_review_pr_after_optional_loop` now returns a skipped non-side-effect dispatch outcome for halted loops even if external actions were requested.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_ -- --nocapture`: pass, 12 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 272 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- No full affected review-loop rerun was executed in this checkpoint because Tier G synthetic source is still `to_author` and Tier C full review-loop execution is a full multi-agent run.
- N5 is now a runtime halt for matched corpus sources; later `agh app eval` work should move this from review-command source matching into first-class eval-run metadata.

## P0-004: Citation Waterfall Not Wired For PR-54 Classics

ID: P0-004
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: none.
Symptom: citation validation checked 53 references and emitted partial evidence, but left 8 unverified; all evidence came from `crossref_bibliographic`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-004-citation-waterfall.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/citation_validation_report.json`
Root cause at audit time: resolver waterfall was not implemented or not wired into this review-loop path; ADS, zbMATH, OpenAlex, INSPIRE, and Gemini-grounded adjudication evidence were absent. P0-012 through P0-014 have since added the verifier-side deterministic waterfall, retraction screening, config-gated grounded fallback, and provider headers; the live Tier R rerun remains the proof step.
Fix plan: add Weyl-classics citation fixture, implement deterministic waterfall/cache, preserve per-reference partial statuses, and require unverified/needs_review count `<= 2`.
Attempts: 1
Escalation status: none.

## P0-004 Progress: PR-54 Classics Resolver Waterfall

Status: partially fixed locally, 2026-06-13T01:47Z.
Evidence:
- Added `bibliographic_waterfall_resolves_pr54_classics_and_keeps_partial_results`.
- The fixture first failed before implementation because `CitationVerifier::with_bibliographic_provider_bases` did not exist.
- Plain references now keep Crossref first, then try OpenAlex, Semantic Scholar, NASA ADS, INSPIRE-HEP, and zbMATH in order.
- Provider lookups use a bounded per-provider timeout and tolerant JSON extraction for title, DOI, and URL/bibcode evidence.
- Final per-reference results are cached and emitted in the existing citation verifier `entries[]` with `source`, `verified_via`, `status`, `resolved_doi`, `resolved_url`, and `reason`.
- The PR-54 classics fixture resolves Cartan/Ehlers/Kunzle-style references via ADS and Trautman via zbMATH, leaves exactly two residues as `unverified`, and emits a non-empty partial-result artifact shape.
- Added `citation_validation_report_preserves_waterfall_resolver_sources` to protect waterfall resolver sources in deterministic citation-validation reports.
- `citation_validation_report.schema.json` now admits waterfall resolver sources and statuses, and the report builder preserves resolver `resolved_doi`, `resolved_url`, and evidence.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier bibliographic_waterfall_resolves_pr54_classics_and_keeps_partial_results -- --nocapture`: pass.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 30 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation_validation -- --nocapture`: pass, 3 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 273 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- No full affected `regression-pr54-weyl` review-loop rerun was executed in this checkpoint, so Tier R is not green yet.
- Retraction screening is now fixed locally by P0-004b.
- Verifier-side Gemini-grounded fallback and ADS/Semantic Scholar auth headers are now fixed locally by P0-004c.
- Repo `.env` still lacks `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL` and provider keys, so the live grounded endpoint and affected Tier R rerun remain pending.

## P0-004 Progress: Local Gemini Grounded API Fallback

Status: fixed locally for the app-local Gemini API transport, 2026-06-13T02:23Z.
Evidence:
- Added `local_gemini_grounded_api_resolves_residue_with_grounding_metadata`.
- The fixture first failed before implementation because `CitationVerifier::with_bibliographic_and_local_gemini_grounded_provider_bases` did not exist.
- The verifier now appends a final `gemini_grounded` provider from local env when `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL` is unset and `GOOGLE_GENERATIVE_AI_API_KEY`, `GEMINI_API_KEY`, or `GOOGLE_API_KEY` is configured.
- The direct Gemini request posts to `/v1beta/models/<model>:generateContent`, sends `x-goog-api-key`, enables `tools: [{"google_search": {}}]`, requests JSON output, and preserves `groundingMetadata.groundingChunks[*].web.uri` as URL evidence.
- Grounded hits still require `verdict`/`status` of `verified`, `resolved`, or `exists`, a matching title, and HTTP URL evidence before they become `status=resolved`, `source=gemini_grounded`, and `verified_via=gemini_grounded`.
- Added `default_providers_include_local_gemini_api_when_key_is_configured` to protect env selection, including model and mockable base URL.
- `agenthero/apps/grokrxiv/env/.env_review.example` now documents `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL`, `GROKRXIV_CITATION_GROUNDED_MODEL`, and `GROKRXIV_CITATION_GROUNDED_GEMINI_BASE_URL`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier grounded -- --nocapture`: pass, 2 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier default_providers_include_local_gemini_api_when_key_is_configured -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 35 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
Residual:
- Repo `.env` and included env files still lack `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL`, `GOOGLE_GENERATIVE_AI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `SEMANTIC_SCHOLAR_API_KEY`, `NASA_ADS_API_TOKEN`, and `ADS_API_TOKEN`, so this checkpoint does not prove a live Tier R affected review-loop run.
- Configure a Gemini API key or app-local grounded resolver endpoint before the next `regression-pr54-weyl` safe rerun.

## P0-004 Live Tier R Rerun: Partial Results But 5 Unverified Residues

ID: P0-004e
Corpus entry: `regression-pr54-weyl`
Review id: `83675683-633c-44a4-b9c6-0569eee2ddeb`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F3 toolchain/config until a real grounded resolver/API key/ADS token is configured; F1 if residue remains above target with configured providers.
NEVER-event: none. Citation artifact was non-empty and partial results were emitted.
Symptom: citation validation now emits a partial-result report and has `unresolved=0`, `malformed=0`, `transient_unknown=0`, but still has `unverified=5` against Tier R `citation_needs_review <= 2`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T023022Z/regression-pr54-weyl/run.log`
- `agenthero/apps/grokrxiv/evals/results/20260613T023022Z/regression-pr54-weyl/verdict.json`
- `agenthero/apps/grokrxiv/evals/results/20260613T023022Z/regression-pr54-weyl/dossier.md`
Artifact paths:
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/83675683-633c-44a4-b9c6-0569eee2ddeb/review_loop/citation_validation_report.json`
Observed counts: `checked=53`, `unverified=5`, `unresolved=0`, `malformed=0`, `transient_unknown=0`, `unresolved_fraction=0.0`.
Unverified keys: `Cartan`, `Ehlers`, `March`, `Reichenbach`, `Trautman`.
Provider evidence for residue: public fallback only; evidence strings report Semantic Scholar `429 Too Many Requests`, ADS `401 Unauthorized`, and zbMATH `400 Bad Request`.
Root cause: repo `.env` and split env files do not configure `GROKRXIV_CITATION_GROUNDED_RESOLVER_URL`, Gemini API key envs, `SEMANTIC_SCHOLAR_API_KEY`, or ADS token envs, so P0-014/P0-015 live final providers were not available in this run.
Owning code: `agenthero/apps/grokrxiv/crates/verifier/src/citation.rs` and local env/config.
Fix plan: configure a real local grounded resolver endpoint, Gemini API key, ADS token, or add another deterministic provider that resolves at least three of the five remaining references with URL evidence. Re-run the same safe Tier R command and require residue `<= 2`.
Attempts: 2 live Tier R runs for citation reliability after initial audit; latest safe run after P0-015 still red.
Escalation status: blocked on local credential/provider configuration or additional deterministic provider work; do not claim P0-004 complete.

## P0-004 Progress: Structured Title Bibliographic Query

Status: fixed locally for verifier behavior, 2026-06-13T02:58Z.
Evidence:
- Added `bibliographic_waterfall_prefers_structured_title_over_raw_label`.
- The verifier now sends `Citation.title` to the bibliographic waterfall when a parsed non-empty title exists, instead of querying with the raw bibliography label such as `Cartan:1986: ...`.
- This lets providers such as OpenAlex match old/classic records by clean title while retaining the raw citation as fallback when no structured title exists.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier bibliographic_waterfall_prefers_structured_title_over_raw_label -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 36 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
Residual:
- PATH `grokrxiv-app` and `agenthero-dag-app-grokrxiv` were reinstalled after this verifier change.
- The affected `regression-pr54-weyl` safe review-loop rerun at `20260613T025743Z` interrupted before citation validation, so the structured-title fix is not yet proven against Tier R.

## P0-018: Affected Rerun Interrupted Before Citation Validation

ID: P0-018
Corpus entry: `regression-pr54-weyl`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: unknown; the command/session ended without usable product output.
finish_reason: zero-byte log; partial review artifact tree existed, but no Haskell result or citation validation artifact was written.
Bucket: F3 toolchain/runtime or session interruption.
NEVER-event: none observed; no citation-validation result was emitted.
Symptom: after reinstalling PATH binaries from commit `39b9a64`, the affected rerun did not flush any product output to `run.log`. It created partial review artifact tree `19197b5c-84cd-4c5f-9693-557943b3dc58` with early review-loop artifacts through `semantic_model.json`, then all run processes exited before `review_loop/haskell/results.json` or `review_loop/citation_validation_report.json` appeared.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T025743Z/regression-pr54-weyl/run.log` (zero bytes)
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/19197b5c-84cd-4c5f-9693-557943b3dc58/review_loop/`
Observed process state: after the session ended, `ps`/`pgrep` showed no remaining `agh`, `agenthero-dag-app-grokrxiv`, `grokrxiv-app`, `claude`, or `codex exec` process for this run.
Root cause: unknown. The partial artifact tree proves the app advanced past review creation, but not far enough to prove P0-004e.
Owning code: unknown; do not change code from this evidence alone.
Fix plan: retry the same safe affected command. If another zero-output interruption occurs, then debug run/session capture or add app-runtime progress logging around Haskell stage execution.
Attempts: 1 after P0-004e structured-title fix.
Escalation status: none; retry needed.

## P0-019: Structured-Title Rerun Improved Citation Residue To 3

ID: P0-019
Corpus entry: `regression-pr54-weyl`
Review id: `9dc53304-6085-4d3b-8009-293ebeebf686`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract/provider integration for citation; F4 cascades for Haskell/Lean/PR/policy.
NEVER-event: none. Citation artifact was non-empty; external actions were disabled and `pr_url=null`.
Symptom: structured-title lookup improved the live Tier R citation residue from `unverified=5` to `unverified=3`, but the expected threshold is `<= 2`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T042403Z/regression-pr54-weyl/run.log`
- `agenthero/apps/grokrxiv/evals/results/20260613T042403Z/regression-pr54-weyl/exit.status`
Artifact paths:
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/9dc53304-6085-4d3b-8009-293ebeebf686/review_loop/citation_validation_report.json`
Observed counts: `checked=53`, `unverified=3`, `unresolved=0`, `transient_unknown=0`.
Unverified keys: `March`, `March`, `Weyl`.
Root cause: live zbMATH requests used `_structured_search?query=...`, which the public API treated as a bad/no-parameter request. Direct diagnostics showed `_search?search_string=Zur%20Infinitesimalgeometrie...` returns the Weyl record with `zbmath_url=https://zbmath.org/2603060`.
Owning code: `agenthero/apps/grokrxiv/crates/verifier/src/citation.rs`
Fix plan: add a red fixture for zbMATH `_search?search_string=...` object-shaped title results, then update the provider URL and parser.
Attempts: 1 after P0-004e.
Escalation status: none; fixed by P0-004f.

## P0-004f Resolution: zbMATH Search Contract

Status: fixed locally and proven by affected Tier R rerun, 2026-06-13T05:25Z.
Evidence:
- Added `zbmath_search_string_resolves_object_title_results`.
- The fixture first failed with `status 404 Not Found`, leaving the Weyl title `unverified`.
- The verifier now defaults zbMATH to `https://api.zbmath.org/v1/document/_search`.
- The zbMATH provider URL now sends `search_string=<title>&results_per_page=5`.
- The parser now accepts object-shaped `title.title` payloads and preserves `zbmath_url` as `resolved_url`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier zbmath_search_string_resolves_object_title_results -- --nocapture`: pass, 1 test.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier`: pass, 37 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass.
- `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked`: pass.
- Affected safe rerun `20260613T045516Z` completed as review `3619ff6a-1a72-4aa0-bb0f-c8bbcacd8cc3` with product exit 0, `external_actions_enabled=false`, and `pr_url=null`.
- `review_loop/citation_validation_report.json` for review `3619ff6a-1a72-4aa0-bb0f-c8bbcacd8cc3` reports `checked=53`, `unverified=2`, `unresolved=0`, `transient_unknown=0`. This satisfies Tier R `citation_needs_review <= 2`.
Residual:
- No full corpus-green claim. The affected run still fails Haskell typed-IR/semantic validation, Lean/proof obligation cascade, semantic adequacy, PR fixer timeout, and policy gate.
- The two remaining citation residues are both March references. They are within the Tier R threshold, so do not work them before higher-priority P0 gate defects unless the corpus tightens.

## P0-020: Review-Loop Math Source Collector Drops Extracted Theorem Artifacts

ID: P0-020
Corpus entry: `regression-pr54-weyl`
Review id: `3619ff6a-1a72-4aa0-bb0f-c8bbcacd8cc3` before fix; `aa69e733-3f72-44e0-af25-136c2b5012b7` after fix
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract/artifact wiring
NEVER-event: related to N1/Tier R extraction completeness, but review did not proceed on an empty body.
Symptom: the persisted paper extraction cache has recovered theorem/equation artifacts, but the review-loop math source artifact drops them. Current cache evidence for `2606.00799`: `body.md` 117,247 bytes, `sections.json` 8 sections, `equations.json` 903 entries, `theorem_graph.json` 41 nodes. The affected review-loop artifact `paper_math_sources.json` recorded empty equation/theorem sources with `reason="not_loaded"`, while stderr summarized `theorem_nodes=0 equations=0 sources=1`.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T045516Z/regression-pr54-weyl/run.log`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/body.md`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/equations.json`
- `/Users/mlong/Documents/Development/grokrxiv-data/papers/2606.00799/theorem_graph.json`
Artifact paths:
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/3619ff6a-1a72-4aa0-bb0f-c8bbcacd8cc3/review_loop/paper_math_sources.json`
Root cause: `paper_math_source_collector` only loaded equation/theorem artifacts through `paper_assets` when the DB asset pointer was `ready`. The prior no-cache extraction materialized valid Tier-1 files, then failed later on the configured data-repo remote push, leaving `paper_assets` non-ready. The collector therefore loaded only the latest `review_inputs.artifact` sections and skipped disk artifacts.
Owning code:
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
Fix:
1. Added a failing fixture where persisted extraction artifacts include non-empty equations and theorem graph, and `paper_math_source_collector` must preserve them from the data-repo cache even when the DB asset pointer is not ready.
2. Added `load_review_loop_paper_math_source_files` and `load_review_loop_paper_math_sources_from_data_repo_cache`.
3. Updated the collector to fall back to `GROKRXIV_DATA_REPO_PATH/papers/<base-arxiv-id>/review_input.json` and load `sections.json`, `body.md`, `equations.json`, and `theorem_graph.json` from the same persisted bundle when `paper_assets` is absent or non-ready.
4. Re-ran the affected Tier R entry and confirmed `paper_math_sources.json` carries non-empty theorem/equation sources.
Attempts: 1 live affected run after P0-004f exposed this gap.
Escalation status: none.

## P0-020 Resolution

Status: fixed locally, 2026-06-13T06:09Z.
Evidence:
- Red fixture: `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime paper_math_source_collector_uses_data_repo_cache_when_asset_pointer_not_ready -- --nocapture` failed before implementation with missing `load_review_loop_paper_math_sources_from_data_repo_cache`.
- Green fixture: same command passed after fix.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop_ -- --nocapture`: pass, 12 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture`: pass, 276 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `git diff --check`: pass.
- PATH installs for `grokrxiv-app` and `agenthero-dag-app-grokrxiv`: pass with existing locked yanked-zip warning only.
- Affected run `agenthero/apps/grokrxiv/evals/results/20260613T053725Z/regression-pr54-weyl/run.log`: product exit 0; review `aa69e733-3f72-44e0-af25-136c2b5012b7`; external actions disabled; `pr_url=null`; `paper_math_source_collector [OK] ... theorem_nodes=41 equations=903 sources=6 warnings=0`.
Residual:
- Overall run still fails Haskell typed-IR/Lean blocking, P0-005 PR fixer timeout, and policy gate recommendation semantics. P0-005 is the next queue item.

## P0-016 Review-Loop Triage After Guardrail Fixes

Corpus entry: `regression-pr54-weyl`
Review id: `83675683-633c-44a4-b9c6-0569eee2ddeb`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: mixed.
NEVER-event: none observed. `external_actions_enabled=false`, `pr_url=null`, and stderr confirms external PR dispatch was skipped.
Positive evidence:
- All five specialists started; summary, novelty, reproducibility, and meta-review passed; technical correctness and citation were explicit warnings.
- `review_loop/bundle_completeness.json` reports `status=pass`, `missing_count=0`, and explicit skip reasons for PR-fixer outputs.
- `citation_validation_report.json` is non-empty and reported through the review-loop final artifacts.
Remaining red stages:
- `haskell_review_fix_code`: `SemanticModel.hs must define typed mathematical IR type MathType` plus related `ClaimIR`, `ProofObligation`, `LeanTarget`, and mapping-function requirements.
- `proof_obligation_generator` and `lean_review_fix_code`: cascade from failed Haskell IR generation.
- `semantic_adequacy_checker`: `status=fail`; 369 mapped statements are `OVERCLAIMED`.
- `pr_fixer`: `CliRunner timed out after 360s for role pr_artifact_fixer`; this confirms P0-005 remains reachable on a now-valid extraction/review path.
- `policy_gate`: fails because meta-review recommendation is `major_revision`, not `accept`; this is stricter than Tier R `expected.recommendation: honest`.
Fix plan:
- Keep Haskell/Lean deterministic statement emission under the P2 typed-IR phase unless P0 explicitly changes the review-loop policy to gate only Tier R integrity expectations.
- Reopen P0-005 as a real timeout on valid inputs after P0-004 is either proven or explicitly blocked on provider configuration.
- Add a policy-gate fixture for Tier R `recommendation: honest` before changing policy behavior.
Escalation status: none; no N5 halt.

## P0-005: PR Fixer Timed Out After 360 Seconds

ID: P0-005
Corpus entry: `regression-pr54-weyl`
Review id: `eca527eb-3930-49e6-a828-66dd64611430`
Runner: `cli`
Command: `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F3 toolchain
NEVER-event: none.
Symptom: `pr_artifact_fixer` timed out after 360 seconds; `pr_fixes.json` reports fixed `review.pdf` was not produced.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/dossier-p0-005-pr-fixer-timeout.md`
- `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/eca527eb-3930-49e6-a828-66dd64611430/review_loop/pr_fixes.json`
Root cause: unknown from this audit; likely downstream of P0-003 because the loop should not enter PR fixing after invalid extraction.
Fix plan: do not tune timeouts yet; fix P0-002 and P0-003 first, then rerun if PR fixing is still reachable on valid extraction.
Attempts: 1
Escalation status: no longer purely deferred; P0-016 confirmed the same timeout on review `83675683-633c-44a4-b9c6-0569eee2ddeb` after extraction, specialists, bundle completeness, and citation partial-result emission were all present. Still work it after P0-004 citation residue is either green or formally blocked by missing local provider credentials.

## P0-005 Resolution

Status: fixed locally, 2026-06-13T07:50Z.
Root cause:
- The rendered `review.tex` had deterministic LaTeX defects before PR fixing: unescaped role slugs such as `meta_reviewer` in section titles and raw PDFLaTeX-hostile Unicode math/combining marks from review text.
- `run_review_loop_pr_fixer` always invoked the `pr_artifact_fixer` agent even when the already-rendered artifact could be compiled deterministically, which fed a large repair prompt and timed out after 360 seconds.
- During verification, installing `agh` and the GrokRxiv adapter alone was insufficient because the adapter launches the app runtime binary `grokrxiv-app`; all three PATH binaries must be refreshed after runtime changes.
Fix:
1. Escaped agent role slugs in LaTeX section titles.
2. Added PDFLaTeX-safe mappings for Greek letters, common math symbols, superscripts, dashes, and combining marks observed in live review artifacts.
3. Added `try_compile_existing_pr_artifact`: the PR fixer first copies rendered `review.tex` to `review_loop/fixed/review.tex`, runs the configured LaTeX compiler with a bounded 120-second timeout, and writes a pass `pr_fixes.json` with zero agent outputs when `review.pdf` is produced.
4. Preserved the existing agent repair path for missing or non-compilable rendered TeX.
Evidence:
- Red fixture `latex_escapes_agent_role_in_section_titles` failed before implementation because raw underscores in `meta_reviewer` were emitted in LaTeX section titles, then passed after escaping.
- Red fixture `pr_fixer_accepts_compilable_rendered_tex_without_agent` failed before implementation because the compile-first helper did not exist, then passed.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-render --test render`: pass, 10 tests.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime pr_fixer_accepts_compilable_rendered_tex_without_agent --lib`: pass.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop`: pass, 12 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- PATH installs passed for `agh`, `agenthero-dag-app-grokrxiv`, and `grokrxiv-app`.
- Affected safe run `agenthero/apps/grokrxiv/evals/results/20260613T072256Z/regression-pr54-weyl/run.log`: product exit 0; review `c0f0e300-2654-4e85-b26c-a50d530e24f0`; external actions disabled; `pr_url=null`; `pr_fixer [OK]`; `pr_review_fix_code [OK]`.
- `review_loop/pr_fixes.json` for review `c0f0e300-2654-4e85-b26c-a50d530e24f0` reports `status=pass`, `fixed_tex=review_loop/fixed/review.tex`, `fixed_pdf=review_loop/fixed/review.pdf`, `compile_review_loop.status=pass`, `author_role=deterministic_pr_artifact_compiler`, `agent_output_audit_summary.total=0`, and first compile attempt `exit_code=0`.
Residual:
- Overall affected run still fails Lean proof-author timeout, semantic adequacy `OVERCLAIMED`, and policy gate requiring `accept` despite Tier R `expected.recommendation: honest`.
- No full corpus-green claim and no phase tag.

## P0-021: Tier R Honest Recommendation Was Treated As Accept-Only Publication Gate Failure

ID: P0-021
Corpus entry: `regression-pr54-weyl`
Review id before fix: `c0f0e300-2654-4e85-b26c-a50d530e24f0`
Review id after fix: `d18f023f-d9ce-4788-b81c-de7f3ba57c16`
Runner: `cli`
Command: `agh --json app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --no-external-actions`
Exit code: 0
finish_reason: product command completed with review-loop `deterministic_status=fail`
Bucket: F1 contract
NEVER-event: none.
Symptom: Tier R expected `recommendation: honest` with verdict unpinned, but the review-loop policy gate added `Meta-review recommendation is `major_revision`, not `accept`.` as a blocking issue. That conflated publisher readiness with corpus integrity and kept the PR-54 regression red for the wrong reason.
Raw evidence paths:
- `agenthero/apps/grokrxiv/evals/results/20260613T072256Z/regression-pr54-weyl/run.log`
- `agenthero/apps/grokrxiv/evals/results/20260613T080031Z/regression-pr54-weyl/run.log`
Artifact paths:
- Before: `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/c0f0e300-2654-4e85-b26c-a50d530e24f0/review_loop/policy_gate.json`
- After: `agenthero/apps/grokrxiv/crates/orchestrator/.agenthero/artifacts/grokrxiv/reviews/d18f023f-d9ce-4788-b81c-de7f3ba57c16/review_loop/policy_gate.json`
Root cause: `ReviewLoopCorpusContext` only carried id/tier/source, so `run_review_loop_for_review` could not see `expected.recommendation: honest`. Policy assembly treated any non-`Pass` `PublicationGate` as a corpus-blocking issue, even when the corpus expectation was an honest non-publishing review rather than acceptance.
Owning code:
- `agenthero/apps/grokrxiv/crates/orchestrator/src/cli.rs`
Fix:
1. Carried `expected.recommendation` from `evals/corpus.yaml` into `ReviewLoopCorpusContext`.
2. Added `review_loop_publication_gate_policy`, separating `publisher_ready` from review-loop `integrity_ready`.
3. For corpus entries with `expected.recommendation: honest`, a valid non-accept recommendation (`minor_revision`, `major_revision`, or `reject`) no longer contributes an accept-only publication-gate blocking issue, while `publisher_ready` remains false.
4. Added `recommendation_policy` evidence to `policy_gate.json`.
Evidence:
- Red fixture `tier_r_honest_recommendation_is_integrity_ready_without_publisher_ready` failed before implementation with missing `expected_recommendation` and `review_loop_publication_gate_policy`.
- Green review-loop test group: `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib review_loop`: pass, 13 tests.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- PATH installs passed for `agh`, `agenthero-dag-app-grokrxiv`, and `grokrxiv-app`.
- Affected safe run `agenthero/apps/grokrxiv/evals/results/20260613T080031Z/regression-pr54-weyl/run.log`: product exit 0; review `d18f023f-d9ce-4788-b81c-de7f3ba57c16`; external actions disabled; `pr_url=null`.
- `review_loop/policy_gate.json` for review `d18f023f-d9ce-4788-b81c-de7f3ba57c16` reports `recommendation_policy.status=honest_non_publishing_recommendation`, `expected_recommendation=honest`, `actual_recommendation=major_revision`, `recommendation_policy.integrity_ready=true`, and `publisher_ready=false`.
- `review_loop_report.json` blocking issues no longer include the accept-only meta-review recommendation reason; remaining issues are Haskell, Lean, and semantic adequacy.
Residual:
- Overall affected run remains red: `haskell_code_fixer` timed out after 360s, proof obligations and Lean were blocked by Haskell, and semantic adequacy remained `OVERCLAIMED`.
- No full corpus-green claim and no phase tag.

## Finding Template

Use one dossier per defect.

```text
ID:
Corpus entry:
Runner:
Command:
Exit code:
finish_reason:
Bucket: F1 contract | F2 fidelity | F3 toolchain | F4 cascade | F5 honest_negative
NEVER-event:
Symptom:
Raw evidence paths:
Artifact paths:
Root cause:
Owning code:
Fix plan:
Attempts:
Escalation status:
```
