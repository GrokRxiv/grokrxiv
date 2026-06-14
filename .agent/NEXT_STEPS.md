# GrokRxiv Local Harness Next Steps

Continue exactly from here. Current instruction is report-only after the single-file `2606.13517` run; do not code or rerun until the user asks.

## Current Coordinator State

- Branch: `grokrxiv-local-corpus-harness`
- Worktree: `/Users/mlong/Documents/Development/grokrxiv`
- Latest merged worker checkpoint: `5c7c31e` (`codex checkpoint: P0 - capset recommendation policy`), fast-forward merged from `p0-050-capset-recommendation-policy`
- Pending worker checkpoint: none.
- Current phase: P0 stabilize, narrowed to the vertical review-pipeline slice.
- Baseline tag: none.
- Last green full sweep: none.
- Run model: local Codex only; do not use Codex Cloud.
- Budget/run rule: do not run the full corpus next. Work one source or one focused fixture at a time.

## Stop Point After P0-054

Do not start a run automatically.

If the user asks to continue coding, use this prompt shape:

```text
Implement only the global citation normalization fix. Do not run the full corpus.
Use the P0-054 findings. Add a failing test at the citation-verifier input boundary proving that quoted-title bibitems, newblock bibitems, amsrefs, .bib/.bbl, and Pandoc bibliography entries pass real titles to the resolver, not citation keys. Fix the normalization/handoff contract. Run focused tests only. Then rerun one affected source with --no-external-actions if explicitly requested.
```

After citation normalization, the second known fix is theorem-target filtering: section-intro prose and truncated statements must not become Lean proof obligations.

## Narrow Acceptance Contract

The near-term goal is:

```text
file/source -> normalized content -> semantic math map -> conditional Haskell/Lean proof path -> LLM review/PR artifact -> git/web evidence report
```

Rules:

- Source and extraction must be reliable. Missing body content fails before any verdict.
- Normalized content must preserve body text, sections, references, math/context artifacts, and provenance.
- Haskell/Lean are conditional proof stages, not universal document stages.
- If normalized content has no formal math targets, Haskell and Lean must be explicit skips with `skip_reason: no_math_targets`; the review/PR artifact path still runs.
- Use `NOT_CONDUCIVE_TO_LEAN_PROOF` as the operator-facing label for the no-math proof skip. Until schemas expose that exact enum, encode it as visible skip artifacts.
- If formal math targets exist, Haskell/Lean must run and emit `PROVED`, `NOT_PROVED`, unsafe proof status, or a classified F1-F5 failure.
- Corpus green means `integrity_ready=true`, not automatic publication. A report can be green while saying not proved, not applicable, reject, or needs review.
- `reference_ready=true` is the public-use bar: the report is good enough for another reader to use as a reference. Claims and limitations must be traceable, unresolved items explicit, and the review useful, readable, and not overclaimed.
- `publisher_ready=true` is stricter: reference-ready plus publication gate passed, recommendation policy allows publication, PR/web artifacts build, and no blockers remain. Real approval/publish actions stay outside the corpus loop.
- LLM agents should not guess what to do with missing data. Every agent call needs an input manifest with required artifacts, optional artifacts, completeness flags, provenance, and explicit missing-data instructions. Missing required data without an allowed skip fails before the LLM call.
- Corpus runs must keep `--no-external-actions`; never invoke approve, request-revisions, publisher, close, withdraw, merge, or PR-opening actions.

## Immediate Queue

### 1. P0-049 Normalized Bibliography / Citation Evidence

Status: accepted and merged to coordinator.

Evidence:

- Root cause: capset uses `amsrefs` `\bib{key}{type}{body}` bibliography entries; previous normalized extraction handled `\bibitem`, `.bib`, and `.bbl` only.
- Code changed: `agenthero/apps/grokrxiv/crates/ingest/src/tex.rs` now parses `amsrefs` bibliography entries, extracting raw text, title, DOI, and arXiv identifiers.
- Red-first fixture: `capset_amsrefs_biblist_entries_are_preserved` failed before implementation with `citations=[]`, then passed.
- Affected result root: `agenthero/apps/grokrxiv/evals/results/20260614T041258Z/capset-after-p0-049-amsrefs`.
- Review id: `f06df5dc-5610-4d4f-a565-3cfccb5a9fe3`.
- Wrapper exit: 0; `run-status.json` says `classification=completed`, `reason=process_exit`, `exit_code=0`, `elapsed_ms=507718`.
- `semantic_ir.json`: `theorem_candidates=0`, `supporting_equations=190`, limitation `no_paper_math_transcribed`.
- Haskell: pass in one deterministic local attempt.
- `proof_obligations.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `obligations=0`.
- `lean/results.json`: `status=skipped`, `skip_reason=no_math_targets`.
- `citation_validation_report.json`: `status=pass`, `checked=7`, `unresolved=0`, `transient_unknown=0`, `unverified=0`, `malformed=0`.

Residual:

- Capset is not green. Policy still has `deterministic_status=fail`, `integrity_ready=false`, `publisher_ready=false`.
- The remaining blocker is policy recommendation semantics: meta-review recommends `major_revision`, and the policy currently treats that as an accept-only integrity failure even though the capset corpus expected block does not specify `expected.recommendation`.
- No full corpus-green claim and no phase tag.

Worker verification:

- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest capset_amsrefs_biblist_entries_are_preserved -- --nocapture`: pass, 1/1.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest --lib -- --nocapture`: pass, 48/48.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21/21.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop -- --nocapture`: pass, 20/20.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21/21.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24/24.
- `cargo fmt --manifest-path agenthero/apps/grokrxiv/Cargo.toml --all`: pass.
- `git diff --check`: pass.

Coordinator verification:

- `git merge --ff-only p0-049-capset-bibliography`: pass, fast-forward to `8cc7686`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest capset_amsrefs_biblist_entries_are_preserved -- --nocapture`: pass, 1/1.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-ingest --lib -- --nocapture`: pass, 48/48.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime citation -- --nocapture`: pass, 21/21.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop -- --nocapture`: pass, 20/20.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21/21.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24/24.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass, refreshed PATH `grokrxiv-app` from merged coordinator checkout.

Next action: start P0-050.

### 2. P0-050 Capset Recommendation Policy Semantics

Status: accepted and merged to coordinator.

Evidence:

- Red-first fixture `unpinned_recommendation_is_integrity_ready_without_publisher_ready` failed before implementation at `assertion failed: policy.integrity_ready`, then passed after the policy change.
- `review_loop_publication_gate_policy` now treats unpinned corpus recommendation expectations as non-publishing integrity passes when the only publication-gate failure is a non-accept meta-review.
- Real publishing remains strict: `publisher_ready=false` unless the publication gate passes.
- Affected result root: `agenthero/apps/grokrxiv/evals/results/20260614T043642Z/capset-after-p0-050-recommendation-policy`.
- Review id: `f94e1367-8924-426c-aaa7-5db84d4dea5b`.
- Wrapper exit: 0; `run-status.json` says `classification=completed`, `reason=process_exit`, `exit_code=0`, `elapsed_ms=765023`.
- `policy_gate.json`: `deterministic_status=pass`, `integrity_ready=true`, `publisher_ready=false`, `blocking_issues=[]`, `recommendation_policy.status=unpinned_non_publishing_recommendation`, `actual_recommendation=major_revision`, `expected_recommendation=null`.
- `citation_validation_report.json`: `status=pass`, `checked=7`, `unresolved=0`, `transient_unknown=0`, `unverified=0`, `malformed=0`.
- `proof_obligations.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `obligations=0`.
- `lean/results.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `proof_status=SKIPPED`, `verdict=NOT_PROVED`.
- External actions stayed disabled and `pr_url=null`.

Worker verification:

- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime unpinned_recommendation_is_integrity_ready_without_publisher_ready -- --nocapture`: red then pass, 1/1.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop -- --nocapture`: pass, 20/20.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21/21.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24/24.
- `cargo fmt --manifest-path agenthero/apps/grokrxiv/Cargo.toml --all`: pass.
- `git diff --check`: pass.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass, installed worker binary for affected rerun.

Coordinator verification:

- `git merge --ff-only p0-050-capset-recommendation-policy`: pass, fast-forward to `5c7c31e`.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime unpinned_recommendation_is_integrity_ready_without_publisher_ready -- --nocapture`: pass, 1/1.
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime review_loop -- --nocapture`: pass, 20/20.
- `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace`: pass.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21/21.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24/24.
- `git diff --check`: pass.
- `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked`: pass, refreshed PATH `grokrxiv-app` from merged coordinator checkout.

Residual:

- No full corpus-green claim and no phase tag.
- The next step is to resume the first bounded full local CLI corpus sweep.

Suggested next command shape:

```bash
cd /Users/mlong/Documents/Development/grokrxiv
git worktree add .agent/worktrees/p0-051-bounded-cli-sweep -b p0-051-bounded-cli-sweep
cd .agent/worktrees/p0-051-bounded-cli-sweep
# run the LOOP.md preflight and bounded CLI corpus sweep with --no-external-actions
```

Then resume the bounded full local CLI sweep from `evals/LOOP.md`.

### 3. P0-044 Acceptance / Merge

Status: accepted and merged to coordinator.

Evidence:

- Result root: `agenthero/apps/grokrxiv/evals/results/20260614T003026Z/zeta3-after-p0-044-acceptance`.
- Review id: `1154e7d0-ea88-48b1-90d5-fd60d5471e59`.
- Product exit: 0; external actions disabled; `pr_url=null`.
- `semantic_category_mapper`: `theorem_candidates=0`, `definitions=0`, `assumptions=0`.
- Haskell: `haskell_review_fix_code [OK]`, `attempts=1`, empty targets/claims/proof obligations in `SemanticModel.hs`.
- Guard strings absent from semantic/Haskell artifacts: `body_math_41`, `body_math_67`, `ReviewCategory`.
- PR artifact path completed; citation validation was non-blocking.

Coordinator verification:

- `grokrxiv-review-loop` tests passed 16/16.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- structural tests passed 45/45.
- `git diff --check` passed.

Next action: already completed by P0-045.

### 2. P0-045 No-Math Proof Skip

Status: accepted and merged to coordinator.

Evidence:

- Result root: `agenthero/apps/grokrxiv/evals/results/20260614T004910Z/zeta3-after-p0-045-no-math-skip`.
- Review id: `849e55d1-b1b8-4c5d-9b53-db9e1aa95007`.
- Product exit: 0; external actions disabled; `pr_url=null`.
- `semantic_category_mapper`: `theorem_candidates=0`, `definitions=0`, `assumptions=0`.
- `proof_obligations.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `obligations=0`.
- `lean/results.json`: `status=skipped`, `skip_reason=no_math_targets`, `verdict=NOT_PROVED`, `proof_status=SKIPPED`, `entries=0`.
- `semantic_adequacy.json`: `status=skipped`, `skip_reason=no_math_targets`, `operator_status=NOT_CONDUCIVE_TO_LEAN_PROOF`, `verdicts=0`.
- `policy_gate.json`: `deterministic_status=pass`, `integrity_ready=true`, `publisher_ready=false`, `blocking_issues=[]`, `publishability_vector.formal=not_conducive_to_lean_proof`.
- Review/PR artifacts built. Live stderr had a display-only `[FAIL] deterministic_status=pass`; source now uses `deterministic_status` for the marker.

Worker verification:

- `grokrxiv-review-loop` tests passed 17/17.
- focused app-runtime no-math skip test passed 1/1.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- `git diff --check` passed.

Coordinator verification:

- `git merge --ff-only p0-045-no-math-proof-skip`: pass, fast-forward to `eaaf4d4`.
- `grokrxiv-review-loop` tests passed 17/17.
- focused app-runtime no-math skip test passed 1/1.
- app-runtime `review_loop` tests passed 19/19.
- app workspace check passed.
- structural tests passed 45/45.
- `git diff --check` passed.
- PATH installs passed for `grokrxiv-app`, `agenthero-dag-app-grokrxiv`, and `agh`.
- Wrapped PATH dry-run passed with `external_actions.enabled=false`.

Next action: already completed by P0-045b; start P0-046 next.

### 2b. P0-045b LLM Input Contract Gate

Status: accepted and merged to coordinator.

Evidence:

- Red-first fixture `review_loop_agent_input_contract_rejects_missing_semantic_ir_before_agent` failed before implementation with missing helper, then passed.
- Missing Haskell semantic IR now blocks before deterministic Haskell generation or LLM runner invocation with `stage=haskell_review_fix_code`, `missing_artifact=review_loop/semantic_ir.json`, and remediation `rerun semantic_category_mapper`.
- Review-loop code-agent payloads include `input_contract` with `missing_required_input_policy=fail_before_llm_call`.
- Worker verification passed: app-runtime `review_loop` 20/20, app workspace check, structural tests 45/45, `git diff --check`, full app-runtime lib serial 295/295, PATH installs, and wrapped dry-run.
- Coordinator verification passed after fast-forward merge to `6700d28`: app-runtime `review_loop` 20/20, app workspace check, structural tests 45/45, full app-runtime lib serial 295/295, `git diff --check`, PATH installs, `agh --version`, and wrapped dry-run with `external_actions.enabled=false`.

Next action: start P0-046.

### 3. P0-046 Harness Timeout Detection

Status: accepted and merged to coordinator.

Evidence:

- Added `agenthero/apps/grokrxiv/evals/bin/grokrxiv-run-with-timeout`.
- `LOOP.md` now wraps corpus entry commands with the bounded helper and writes `run-status.json` next to `run.log`.
- Wall timeout emits `bucket=F3`, `classification=timeout`, `reason=wall_timeout`, exit 124.
- Idle-log stall emits `bucket=F3`, `classification=stall`, `reason=idle_timeout`, exit 124.
- Status JSON records command, PID, process state, elapsed time, exit code or signal, raw log path, last log line, and log mtime.
- Worker verification passed: focused corpus tests 11/11; app-runtime `review_loop` 20/20; app workspace check; structural tests 45/45; `git diff --check`; successful-wrapper smoke.
- Coordinator verification passed after fast-forward merge to `d373291`: focused corpus tests 11/11; app-runtime `review_loop` 20/20; app workspace check; structural tests 45/45; `git diff --check`; successful-wrapper smoke.

Next action: start the first bounded full local CLI corpus sweep.

### 4. P0-047 Withdrawn Bertrand Runtime Skip

Status: accepted and merged to coordinator.

Evidence:

- Worker branch: `p0-047-withdrawn-source-skip`.
- Worker commit: `42b5f8f`.
- Coordinator merge commit: `1f26c05`.
- The CLI now reads the corpus skip contract before DB/supervisor/extraction/review work.
- `https://arxiv.org/abs/2407.07620v5` emits:
  - `source_status=withdrawn_unavailable`
  - `extraction=skipped_withdrawn_source`
  - `review_loop=skipped_before_review`
  - `skip_reason=withdrawn_or_unavailable_source`
- The skip is version-specific: the fixture asserts `2407.07620v4` does not inherit the withdrawn v5 skip.
- Merged PATH acceptance root: `agenthero/apps/grokrxiv/evals/results/20260614T-p0-047-merged/bertrand-elementary/`.
- `run-status.json`: `classification=completed`, `exit_code=0`, `elapsed_ms=1008`.
- Negative evidence: no `Extract`, `vlm starting`, `review_id=`, or `publication policy` markers in the run log.

Verification:

- Focused red/green app-runtime fixture passed.
- Corpus app-runtime tests passed 12/12.
- App workspace check passed.
- Structural tests passed 45/45.
- `git diff --check` passed.
- PATH installs passed for `grokrxiv-app`, `agenthero-dag-app-grokrxiv`, and `agh`.
- Coordinator focused test passed after merge.

Next action: start the first bounded full local CLI corpus sweep.

### 5. First Bounded Full Local CLI Corpus Sweep

Run LOOP.md preflight and corpus entries through `grokrxiv-run-with-timeout`.
Use the generated `run-status.json` for F3 classification instead of manual
stall diagnosis. Keep `--no-external-actions`.

Triage rules:

- `bertrand-elementary` is expected to skip before review as withdrawn/unavailable v5. If it starts extraction, triage as P0-047 regression.
- `zeta3-irrationality` should no longer be blocked by P0-044/P0-045/P0-046; if citation timeout reappears, triage it with wrapper evidence.
- No full P0 green claim until all entries pass, zero NEVER-events, structural tests stay green, and the sweep is repeated on both runners.

### 6. P0-039 Withdrawn Bertrand Source

Resolved by human sign-off on 2026-06-14:

- Keep `bertrand-elementary` pinned to withdrawn/unavailable `2407.07620v5`.
- Do not review it.
- Treat the expected outcome as a source/extraction skip:
  `source_status: withdrawn_unavailable`,
  `extraction: skipped_withdrawn_source`,
  `review_loop: skipped_before_review`,
  `skip_reason: withdrawn_or_unavailable_source`.

A retrievable `v4` replacement can be added later as a separate corpus decision.

## Resume Prompt

```text
Read .agent/AGENT_STATUS.md, .agent/FINDINGS.md, .agent/PATCH_PLAN.md,
.agent/TEST_LOG.md, .agent/NEXT_STEPS.md,
agenthero/apps/grokrxiv/evals/PHASES.md,
agenthero/apps/grokrxiv/evals/LOOP.md, and
agenthero/apps/grokrxiv/evals/results/LEDGER.md.

Continue the local-only P0 vertical slice:
file/source -> normalized content -> semantic math map -> conditional
Haskell/Lean proof path -> LLM review/PR artifact -> git/web evidence report.

Do not run the full corpus next. Run only the current failed non-skipped entry:
`pfr-marton` / `https://arxiv.org/abs/2311.05762v2`.

Use `agenthero/apps/grokrxiv/evals/bin/grokrxiv-run-with-timeout` with
`--no-external-actions`. The P0-052 patch at `d19f071` is already merged to the
coordinator and installed PATH `grokrxiv-app` before cleanup, but the app target
directory was removed to reclaim disk; rebuild/install only if the PATH binary
is stale.

Acceptance focus for PFR:
- citation LLM timeout must not erase deterministic checked citation verifier
  evidence;
- citation validation must emit checked/residue counts, not an empty artifact;
- PR artifact fixer must receive initial compile stderr/stdout and produce a
  recompilable PDF or fail with explicit diagnostics;
- no approve/publish/request-revisions actions.
```

## Cleanup Note

`.agent/worktrees/*`, `agenthero/apps/c2rust/target`, and
`agenthero/apps/grokrxiv/target` were removed on 2026-06-14. Durable `.agent/*.md`
files were preserved.

## Current Next Step After Single-File Run

Do not run the full corpus next.

The active single-file target is `https://arxiv.org/abs/2606.13517`, result root
`agenthero/apps/grokrxiv/evals/results/20260614T064246Z/arxiv-2606-13517-single/`,
review `959b4087-f8c6-41ea-8337-01855c2bc2c2`.

Work one defect at a time, then rerun only this same source with
`--no-external-actions`:

1. Fix normalized bibliography title extraction for references whose raw text is
   `Key: Author, ``Title'', ...`; current citation validation checked 50 refs
   but left 34 unverified because many titles are just keys like `Aki01`.
2. Fix theorem target filtering so section-heading prose is not promoted into a
   Lean proof obligation; current Lean failed honestly on `thm_4` derived from
   section-heading prose.
3. Then address agent payload latency if still needed: HTML quality receives the
   full rendered HTML and Lean review receives >1 MB inputs on this paper.

Rerun shape:

```sh
agenthero/apps/grokrxiv/evals/bin/grokrxiv-run-with-timeout \
  --timeout-secs 1800 \
  --idle-timeout-secs 600 \
  --status-json agenthero/apps/grokrxiv/evals/results/<ts>/arxiv-2606-13517-after-fix/run-status.json \
  --log agenthero/apps/grokrxiv/evals/results/<ts>/arxiv-2606-13517-after-fix/run.log \
  -- \
  agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env \
  agh --json app run grokrxiv review https://arxiv.org/abs/2606.13517 --loop --debug --no-external-actions
```
