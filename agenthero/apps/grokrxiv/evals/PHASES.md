# GrokRxiv Local Phased Build

This file is the local Codex harness contract for the multi-day GrokRxiv review-pipeline build. It is app-owned eval documentation, not Codex Cloud state. The goal is to make the review pipeline reliable and trustworthy in the PR-54 sense first, then use the same corpus gate to ratchet through deterministic Lean emission, platform workflow primitives, durable execution, sandboxing, a second app, and native eval productization.

## Authority

Read these files before doing any work:

- `agenthero/apps/grokrxiv/evals/corpus.yaml`: golden corpus, expected blocks, and NEVER-events.
- `agenthero/apps/grokrxiv/evals/LOOP.md`: run, check, dev, fix procedure.
- `.agent/AGENT_STATUS.md`: current phase, runner, sweep state, baseline tag, and in-flight defect.
- `.agent/FINDINGS.md`: defect dossiers, evidence, and any proposed expectation changes requiring human sign-off.
- `.agent/PATCH_PLAN.md`: ordered defect queue; work top-down unless the coordinator edits it.
- `.agent/TEST_LOG.md`: command evidence, exit status, and raw-log pointers.
- `.agent/NEXT_STEPS.md`: exact continuation prompt.
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`: append-only phase and sweep history.

Persist state in files, not chat memory:

- `.agent/AGENT_STATUS.md`
- `.agent/FINDINGS.md`
- `.agent/PATCH_PLAN.md`
- `.agent/TEST_LOG.md`
- `.agent/NEXT_STEPS.md`
- `agenthero/apps/grokrxiv/evals/results/LEDGER.md`

## Local Harness Model

- Coordinator: one local Codex session in `/Users/mlong/Documents/Development/grokrxiv`.
- Workers: local Codex sessions only, launched for isolated defects or subsystems.
- Isolation: worker sessions use local git worktrees under `.agent/worktrees/<phase>-<defect-or-team>/`.
- No cloud: do not use `codex cloud exec`, `codex cloud`, `codex apply`, or cloud task state.
- Resume locally with `codex resume` from this checkout, or start a fresh local Codex session and read the authority files above.
- No publishing: corpus loop sessions must not invoke `approve`, `request-revisions`, publisher, merge, or external publishing actions.

Worker startup:

```bash
cd /Users/mlong/Documents/Development/grokrxiv
git worktree add .agent/worktrees/p0-n1-extraction -b p0-n1-extraction HEAD
cd .agent/worktrees/p0-n1-extraction
codex
```

Worker prompt:

```text
You are a local Codex harness worker, not a cloud task.
Read corpus.yaml, LOOP.md, PHASES.md, .agent state, and LEDGER.md first.
Work on exactly one defect from .agent/PATCH_PLAN.md.
Do not weaken corpus expected blocks or NEVER-events.
Do not invoke approve/request-revisions/publisher actions.
Persist findings, tests, raw logs, and next steps to .agent files.
Use TDD: failing fixture test -> minimal fix -> affected corpus rerun.
End with git status and a checkpoint commit.
```

## Agent Teams

- Coordinator / Integrator: phase state, corpus sweeps, merge order, baseline tags, ledger, and branch hygiene.
- Corpus Auditor: audit-only RUN+CHECK, F1-F5 triage, no patches.
- Gate Worker: N1-N5 app-local gate defects.
- Citation Worker: resolver waterfall, cache, partial results, chunked timeouts, per-reference statuses, retraction checks, grounded fallback.
- IR / Proof Worker: typed IR, Haskell round trip, deterministic Lean statements, proof-completer validation, verdict taxonomy.
- Platform Worker: app-agnostic P1/P3/P4/P5 root-crate work only.
- Verifier Worker: independent review of raw logs, test evidence, and expectation integrity.

## Phase Run Units

Every phase is decomposed into local runs so no single terminal session carries the whole plan:

1. Coordinator refresh: read authority files, `git status --short --branch`, verify current phase and baseline tag.
2. Audit run: Corpus Auditor executes `LOOP.md` RUN+CHECK without patches, starting with the highest-priority regression entry. All failures become F1-F5 dossiers.
3. Patch shards: Coordinator assigns exactly one defect at a time to Gate, Citation, IR/Proof, or Platform workers in local worktrees.
4. TDD fix run: worker writes the failing fixture, proves it fails, implements the minimal fix, reruns the fixture and affected corpus entry, and commits.
5. Verifier run: Verifier Worker checks raw logs, exit codes, artifact paths, schema validity, and expectation integrity before integration.
6. Integration run: Coordinator merges accepted local worker branches in dependency order, reruns affected entries, updates `.agent/*` and `LEDGER.md`, and commits.
7. Phase exit run: only when the phase might be done, run two full-corpus sweeps on both local runners plus structural tests, then tag `phase<N>-green`.

If a worker hits the same defect three times without convergence, stop that worker, write the dossier, and return it to the coordinator. Do not grind it down by silently widening limits or weakening corpus expectations.

## Gate Mechanics

- Never start a phase on red.
- Entry condition: previous phase exit sweep is green, tagged, and recorded with git SHA, corpus verdicts, runner, and provenance in `LEDGER.md`.
- Exit condition: two consecutive full-corpus sweeps on both local runners, zero NEVER-events, phase expectations enabled and passing, and structural tests green.
- Structural tests currently include the 45 tests in `dag_app_registry` and `agenthero_cli_contract`; keep them green throughout and expand the list when new platform contracts land.
- Corpus monotonicity: add entries or tighten expectations only. Never edit an `expected:` block or `never_event` to turn red green. Proposed weakening requires human sign-off and stops that thread.
- N5 halt: Lean `PROVED` on Tier C/G flawed or false claims halts all workers until a dossier is reviewed.
- Evidence rule: cite raw output, exit code, finish_reason, and artifact path. Do not mask failures by raising token caps or timeouts without a diagnosed cause.
- CLI rule: after CLI changes, run `cargo install --path crates/orchestrator --force --locked` and verify the PATH `agh` binary with `agh --version`.

## Golden Corpus Fix Discipline

The golden corpus defines the work, not chat memory. The loop fixes deterministic, mechanically checkable, app-local failures first:

- N1-N3 gate defects: empty-body reviews, silent specialist loss, and verdicts from incomplete inputs.
- Citation reliability: resolver waterfall, partial-result emission, chunked timeouts, per-reference statuses, and Tier R `needs_review <= 2`.
- Contract drift: prompt, schema, model, and artifact breaks caught before merge.

The loop flags but does not autonomously solve architecture gaps, missing corpus coverage, quality depth, or toolchain flakes. Architectural items become phase work only when a human has commissioned them in this file. Production failures must be added as Tier R entries before the loop can protect against them.

Golden corpus check order is fixed by `LOOP.md`: NEVER-events, artifact presence, schema validity, independent `ghc`/`lake`/DB/grep re-verification, then expected-block diff. Failures are classified F1-F5, and implementation sessions work:

```text
one defect -> failing fixture test -> minimal fix -> affected corpus entry rerun -> checkpoint commit
```

Full sweeps are reserved for phase-entry validation, suspected phase completion, and coordinator integration checkpoints; narrow worker loops rerun the affected fixture and corpus entry first.

## Near-Term Vertical Slice

The immediate P0/P2 bridge is narrower than the full roadmap. The app must first make this path reliable:

```text
file/source -> normalized content -> semantic math map -> conditional Haskell/Lean proof path -> LLM review/PR artifact -> git/web evidence report
```

Stage contracts:

1. Source pull: resolve the requested file, local TeX/PDF, or pinned arXiv version. If the requested source is withdrawn/unavailable and the corpus expects `skipped_withdrawn_source`, skip before review with that reason. Otherwise, a source that cannot produce body content fails at extraction completeness before any review verdict.
2. Normalize/extract: produce reviewable document content with body text, sections, references, equations, theorem-like blocks, and provenance. The extraction report must say what was recovered and what was not.
3. Math eligibility: decide from normalized content whether there are formal math targets. Equations, bibliography snippets, review prose, prompt-injection text, and semantic gaps are context unless promoted by a theorem-like source with enough structure.
4. Haskell semantic map: when math targets exist, mechanically build the semantic map from extracted content and run GHC/reviewer checks. When no math targets exist, skip Haskell with an explicit `skip_reason` and continue the document review path.
5. Lean proof verdict: when Haskell emits proof obligations, run Lean and emit a machine verdict. When no math targets exist, skip Lean with an explicit `skip_reason` and continue the document review path.
6. LLM review/PR loop: always review the document content and verifier artifacts. The loop may produce a PR-ready artifact/report, but corpus runs must not publish or open PRs.
7. Git/web report: produce durable artifacts showing which stages passed, failed, or were skipped, with raw evidence paths and no hidden side effects.

Proof-stage verdict meanings:

- `PROVED`: a formal target existed and Lean proved it without forbidden terms.
- `NOT_PROVED`: a formal target existed, but Lean did not prove it.
- `NOT_CONDUCIVE_TO_LEAN_PROOF`: normalized content has no extractable formal math target, so Haskell/Lean are intentionally skipped and the PR/review path continues.
- `USES_SORRY`, `USES_UNAPPROVED_AXIOM`, or equivalent unsafe proof statuses: a formal target exists, but the proof is not acceptable.
- `FAILED`: toolchain, schema, timeout, or runtime failure; this is not a mathematical verdict.

Until every artifact schema has an explicit `NOT_CONDUCIVE_TO_LEAN_PROOF` enum, the implementation may encode the condition as proof-stage skip artifacts with `skip_reason: no_math_targets`. That compatibility shim must be visible in the git/web report and should be removed once the schema is widened.

## Baseline Context

Already landed before this phased plan: app-sdk extraction, retry/backoff hardening, review-loop crate extraction, AgentInput purge, and paper-math IR sourcing in flight. Treat these as baseline context, not active phase scope, and verify current files before relying on any prior implementation detail.

## Phase Sequence

Order:

```text
P0 stabilize -> P2 typed IR / Lean emission -> P3 dynamic nodes -> P4 durable -> P5 sandbox -> P6 c2safe-rust -> P7 agentops
       \-> P1 contract hardening can run in parallel with P0 bake, but cannot exit on a red baseline
```

P2 comes before P3 because trust precedes workflow convenience: deterministic emission changes what the loop verifies; node migration should preserve already-checkable outputs. P6 may start any time after P3 if the generality proof is useful earlier, but any required platform change escapes back to P3-P5.

### P0 Stabilize

Goal: make the corpus able to gate the app.

Scope:

- N1 extraction-completeness gate.
- N2 explicit specialist-failure artifacts.
- N3 gate input completeness.
- N4 bundle completeness.
- N5 fake-proof halt.
- Citation resolver waterfall with caching: Crossref, OpenAlex, Semantic Scholar, NASA ADS, INSPIRE-HEP, zbMATH Open, then Gemini-grounded fallback for residue only.
- Retraction screen using Crossref retraction metadata and Retraction Watch evidence where available.
- Chunked citation fan-out with per-chunk timeouts, per-reference status enums, and partial-result emission.
- Tier E/F/G synthetic papers.
- Pinned `lake`, Lean/mathlib, `ghc`, and arXiv versions.

Exit:

- First full green baseline on both local runners.
- Tier R `regression-pr54-weyl` passes the PR-54 integrity expectations.
- Tier E/F/G entries exist and are live, including the first live N5 false-theorem test.
- Tag `phase0-green`.

### P1 Contract Hardening

Goal: broken contracts fail at registration, not runtime.

Scope:

- `app.yaml` API/version validation.
- Registration-time validation closure for every DAG role, tool, schema, prompt, and node-input dependency.
- Invalid apps listed with reasons in `agh app list` and `agh doctor`.
- Remove `GROKRXIV_BIND`.
- Add a deliberately broken fixture app structural test.

Exit:

- Corpus verdicts unchanged and green.
- Structural tests remain green.
- Deliberately broken app is reported invalid during discovery with actionable reasons.

### P2 Typed IR And Lean Trust

Goal: agents supply proofs only; statements are deterministic.

Scope:

- Expand `semantic_ir.schema.json` to typed `Term`, `Proposition`, `MathType`, and `Unknown*` holes.
- Add transcriber agent with typed IR output schema.
- Make Haskell consume IR JSON and execute JSON -> Haskell -> JSON byte-equal round trip, replacing `-fno-code`-only confidence.
- Add `emitType`, `emitTerm`, `emitProp`, and `emitTheorem` in the review-loop crate to emit Lean theorem statements from IR with `sorry` placeholders.
- Restrict Lean agent to proof completion and byte-identical statement validation.
- Add verdict taxonomy including `PROVED`, `USES_SORRY`, `USES_AXIOM`, `NOT_PROVED`, and `SEMANTIC_GAP`.
- Run `#print axioms` against an allowlist.
- Upgrade adequacy to `MATCHES`, `NARROWED`, and `OVERCLAIMED`.

Exit:

- Tier B statement fidelity becomes mechanical.
- Tier A still proves or honestly reports unproven according to the corpus.
- Tier G tightens to the adequacy taxonomy.

### P3 Dynamic Workflow Primitives

Goal: loops, fan-out, and escalation become platform node kinds.

Scope:

- Add platform `loop`, `map`, `approval`, and `branch` node kinds.
- Honor concurrency.
- Migrate app-buried Haskell, Lean, and PR fix loops onto platform loop nodes.
- Route escalations through approval nodes.

Exit:

- Verdicts and artifacts identical to the P2 tag.
- Per-round events exist for every fix loop.
- Approval-path test pauses instead of failing.

### P4 Durable Execution

Goal: multi-day runs survive crashes and external side effects are exactly-once.

Scope:

- Adapter protocol v2 NDJSON events: `node_started`, `node_finished`, `checkpoint`, `log`, `approval_request`.
- Idempotency keys on requests and external actions.
- Resume tokens and incremental node-state persistence.
- Real per-node timestamps.
- Kill-mid-run local chaos check.
- NEVER-event N6: resumed runs never re-execute completed external side effects.

Exit:

- Resumed and uninterrupted runs match.

### P5 Sandboxed Verification

Goal: proof and compile checks are hermetic.

Scope:

- Sandbox executor for `ghc`, `lake`/Lean, and TeX.
- Pinned images, read-only inputs, network policy, and resource limits.
- Provenance records image digests.

Exit:

- Sandbox verdicts match the P4 host-toolchain tag.

### P6 C2SafeRust Generality Proof

Goal: prove the platform works for a second app without new platform changes.

Scope:

- Build C2SafeRust on the P3-P5 primitives.
- Add a mini-corpus with pinned sample C projects and expected migration outcomes.

Exit:

- GrokRxiv corpus remains green.
- Root `crates/` diff stays empty for the entire phase; if C2SafeRust needs platform changes, stop and triage them as P3-P5 escape work.

### P7 AgentOps Productization

Goal: turn the manual loop into a platform feature.

Scope:

- Implement local `agh app eval grokrxiv`.
- Reproduce `LOOP.md` verdicts exactly.
- Add semantic traces, cost rollups, ledger generation, and StateSchema.

Exit:

- `agh app eval grokrxiv` becomes the canonical corpus loop entrypoint.

## Session Ritual

Start:

```bash
cd /Users/mlong/Documents/Development/grokrxiv
git status --short --branch
```

Resume:

```text
Read .agent/AGENT_STATUS.md, .agent/FINDINGS.md, .agent/PATCH_PLAN.md,
.agent/TEST_LOG.md, .agent/NEXT_STEPS.md, and
agenthero/apps/grokrxiv/evals/results/LEDGER.md.
Continue exactly from NEXT_STEPS.md.
Re-verify the baseline tag is still green before resuming any in-flight fix.
```

End:

```bash
git status
git add .
git commit -m "codex checkpoint: P<N> - <one-line state>"
```

Phase exit:

```bash
git tag phase<N>-green
```
