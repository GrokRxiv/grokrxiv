# GrokRxiv Local Harness Status

Updated: 2026-06-13T01:47:15Z

## Current State

- Goal: Multi-day phased local Codex build of the GrokRxiv review pipeline on AgentHero, gated by the golden corpus.
- Current phase: P0 stabilize.
- Session type: P0 session 12, P0-012 citation waterfall.
- Branch/worktree: `p0-012-citation-waterfall` in `/Users/mlong/Documents/Development/grokrxiv/.agent/worktrees/p0-012-citation-waterfall`.
- Branch base commit: `404693d`.
- Baseline tag: none yet.
- Last green sweep: none yet.
- Current runner: local `cli` first; local `api` runner command must be locked during P0 audit before any two-runner green claim.
- In-flight defect: P0-004 citation reliability. P0-012 fixed the app verifier's deterministic bibliographic waterfall for PR-54-style pre-DOI classics: Crossref weak/noisy matches now flow to OpenAlex, Semantic Scholar, NASA ADS, INSPIRE-HEP, and zbMATH with per-provider timeouts, title normalization/transliteration, cached final per-reference status, and `verified_via` evidence. Residual P0-004 work remains: retraction screening, Gemini-grounded fallback/quorum for unresolved residue, and an affected Tier R rerun proving `needs_review <= 2`. No full corpus green claim yet.
- Run model: local Codex only. Do not use Codex Cloud tasks, cloud apply, or cloud state.
- Agent-team model: coordinator plus local worktree workers; one defect per worker branch and checkpoint commit.

## Ground Truth Files

- `agenthero/apps/grokrxiv/evals/corpus.yaml`
- `agenthero/apps/grokrxiv/evals/LOOP.md`
- `agenthero/apps/grokrxiv/evals/PHASES.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `.agent/NEXT_STEPS.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

## Baseline Validation

- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21 tests, 2026-06-12 before this branch.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests, 2026-06-12 before this branch.
- `cargo test -p agenthero-orchestrator --test dag_app_registry`: pass, 21 tests, 2026-06-12T23:01Z on this branch after harness bootstrap files.
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract`: pass, 24 tests, 2026-06-12T23:01Z on this branch after harness bootstrap files.
- `PHASES.md`: expanded, 2026-06-12T23:19Z, to include local-only phase run units, agent-team handoffs, golden-corpus fix discipline, and the 45 structural-test gate.
- P0 preflight, 2026-06-12T23:21Z: `agh doctor`, `agh --version`, `ghc --version`, `lake --version`, and `lean --version` all exited 0. Raw logs in `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/`.
- P0 first RUN, 2026-06-12T23:24Z: `regression-pr54-weyl` failed before review start because installed `/Users/mlong/.cargo/bin/grokrxiv-app` rejects `--loop`; classified P0-001 / F3.
- P0-001 fix, 2026-06-12T23:27Z: reinstalled `grokrxiv-app` and `agenthero-dag-app-grokrxiv`; product dry-run accepted `--loop` and emitted the review-loop stage plan.
- P0 second RUN, 2026-06-12T23:47Z: `regression-pr54-weyl` completed as review `eca527eb-3930-49e6-a828-66dd64611430`; review-loop deterministic status failed and opened PR #55. New findings: P0-002 no-publishing guardrail breach, P0-003 N1 extraction gate failure, P0-004 citation waterfall gap, P0-005 PR fixer timeout.
- P0-002 fix, 2026-06-13T00:00Z: added `--no-external-actions` to the GrokRxiv review action, app runtime parser, post-loop PR dispatch, dry-run output, help/catalog tests, and LOOP.md. PATH dry-run confirms `external_actions.enabled=false` without starting the DAG.
- P0-003 fix, 2026-06-13T00:10Z: added an app-runtime extraction-completeness gate before review row creation/specialist launch. Safe affected-entry rerun exits 1 at `[2/6] Extract [FAIL] extraction completeness failed`; no `pr_url`, GitHub URL, Review DAG, specialist, or external action output appears in the log.
- P0-006 fix, 2026-06-13T00:28Z: TeX bundle parsing now fails closed when Pandoc/LaTeXML produce no Markdown, source-to-body reports `failed` when `body.md` is empty, and extraction audit treats failed stages as failures. No-cache, no-VLM affected extraction regenerated local artifacts with `body.md` 50,697 bytes and 5 sections; command still exits 1 later on configured data-repo SSH push (`unsupported URL protocol`), which is not fixed in this patch.
- P0-007 fix, 2026-06-13T00:49Z: raw TeX fallback recovers reviewable Markdown from TeX document bodies after converter failure, canonicalizes `\newtheorem` aliases, includes `construction` theorem-like blocks, and reports `source_to_body.tool=raw_tex_markdown_fallback`. Affected extraction for `2606.00799` materialized local artifacts with `body.md` 117,247 bytes, `equations.json` 903 entries, and `theorem_graph.json` 41 nodes. `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace` passed.
- P0-008 fix, 2026-06-13T00:59Z: specialist runner failures now carry an execution-failure marker through review DAG persistence, force verifier status `fail`, and add structured `agent_execution.status=failed`, `role`, and `reason` notes to the rendered agent artifact envelope. `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture` passed, 263 tests; `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace` passed.
- P0-009 fix, 2026-06-13T01:08Z: specialist gate input completeness now uses DAG-declared required specialist roles for both live review DAG gating and persisted publication-gate reconstruction. Missing required roles are represented as blocked roles and force `meta_can_run=false` even when the persisted usable-row count reaches quorum. `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --nocapture` passed, 264 tests; `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace` passed.
- P0-010 fix, 2026-06-13T01:21Z: review-loop bundle completeness now checks manifest-declared artifact outputs, writes `bundle_completeness.json`, records explicit skips for currently unwired citation adjudication and missing PR PDFs, gates policy on unskipped missing outputs, and builds PR attachments from the manifest output list plus harness evidence. Targeted N4 tests passed, serial full app-runtime lib tests passed, and app workspace check passed. Parallel full lib runs exposed pre-existing config/env test isolation flakes; the failing tests passed individually and in the serial full run.
- P0-011 fix, 2026-06-13T01:34Z: N5 false-proof halt now checks corpus Tier C/G context before downstream review-loop work. Lean `PROVED` on `blum-pvnp`/synthetic false-theorem-style entries produces a halt dossier, halted policy/report artifacts, and no PR side effect. Targeted review-loop tests passed, serial full app-runtime lib tests passed, and app workspace check passed.
- P0-012 progress, 2026-06-13T01:47Z: citation verifier now has a deterministic bibliographic resolver waterfall after Crossref for plain references. The new PR-54 classics fixture first failed because the provider-base constructor did not exist, then passed with ADS/zbMATH resolving four of six classic refs and only two unverified residues. Citation validation reports now preserve `ads`/`zbmath` sources, resolved DOI/URL evidence, and expanded resolver statuses. `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-verifier` passed, 30 tests; `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml -p grokrxiv-app-runtime --lib -- --test-threads=1 --nocapture` passed, 273 tests; `cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace` passed.

## Coordinator Rules

- Persist state in files, not chat memory.
- Do not weaken `expected:` blocks or `never_events` to make red runs green.
- Do not invoke `approve`, `request-revisions`, publisher, or merge actions from corpus loop sessions.
- Stop immediately on N5 and write a human escalation dossier.
- End every local session with state updates, ledger append, `git status`, and a checkpoint commit.
