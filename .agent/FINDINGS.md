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
Root cause: resolver waterfall is not implemented or not wired into this review-loop path; ADS, zbMATH, OpenAlex, INSPIRE, and Gemini-grounded adjudication evidence are absent.
Fix plan: add Weyl-classics citation fixture, implement deterministic waterfall/cache, preserve per-reference partial statuses, and require unverified/needs_review count `<= 2`.
Attempts: 1
Escalation status: none.

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
Escalation status: deferred.

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
