# GrokRxiv Local Phased Build

This file is the local Codex harness contract for the multi-day GrokRxiv review-pipeline build. It is app-owned eval documentation, not Codex Cloud state.

## Authority

Read these files before doing any work:

- `agenthero/apps/grokrxiv/evals/corpus.yaml`: golden corpus, expected blocks, and NEVER-events.
- `agenthero/apps/grokrxiv/evals/LOOP.md`: run, check, dev, fix procedure.
- `.agent/AGENT_STATUS.md`: current phase, runner, sweep state, baseline tag, and in-flight defect.
- `.agent/NEXT_STEPS.md`: exact continuation prompt.

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
Read corpus.yaml, LOOP.md, PHASES.md, and .agent state first.
Work on exactly one defect from .agent/PATCH_PLAN.md.
Do not weaken corpus expected blocks or NEVER-events.
Do not invoke approve/request-revisions/publisher actions.
Persist findings, tests, raw logs, and next steps to .agent files.
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

## Gate Mechanics

- Never start a phase on red.
- Entry condition: previous phase exit sweep is green, tagged, and recorded with git SHA, corpus verdicts, runner, and provenance in `LEDGER.md`.
- Exit condition: two consecutive full-corpus sweeps on both local runners, zero NEVER-events, phase expectations enabled and passing, and structural tests green.
- Corpus monotonicity: add entries or tighten expectations only. Never edit an `expected:` block or `never_event` to turn red green. Proposed weakening requires human sign-off and stops that thread.
- N5 halt: Lean `PROVED` on Tier C/G flawed or false claims halts all workers until a dossier is reviewed.
- Evidence rule: cite raw output, exit code, finish_reason, and artifact path. Do not mask failures by raising token caps or timeouts without a diagnosed cause.
- CLI rule: after CLI changes, run `cargo install --path crates/orchestrator --force --locked` and verify the PATH `agh` binary with `agh --version`.

## Phase Sequence

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

### P2 Typed IR And Lean Trust

Goal: agents supply proofs only; statements are deterministic.

Scope:

- Expand `semantic_ir.schema.json` to typed `Term`, `Proposition`, `MathType`, and `Unknown*` holes.
- Add transcriber agent with typed IR output schema.
- Make Haskell consume IR JSON and execute JSON -> Haskell -> JSON byte-equal round trip.
- Emit Lean theorem statements from IR with `sorry` placeholders.
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
- Root `crates/` diff stays empty for the entire phase.

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
