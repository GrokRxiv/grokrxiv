# AgentHero Platform Completion Audit

Date: 2026-06-25

Branch: `agenthero-runtime-kernel`

Scope: current local AgentHero app-run platform, with GrokRxiv as the primary
proof app and `c2rust`, `formal-proofs`, and `platform-smoke` as cross-app
acceptance coverage.

This audit tracks whether AgentHero has become the durable Rust/Tokio harness
for trustworthy DAG workloads. It should be read together with
`docs/agenthero-observability-operator-guide.md`, which documents the operator
surface for status, events, logs, traces, metrics, replay, and no process
inspection.

## Status Vocabulary

| Status | Meaning |
| --- | --- |
| Implemented | The local platform slice has code, tests, and at least one app-run smoke proving the behavior. |
| Partial | The foundation exists, but the current implementation does not yet prove the full stated scope. |
| Pending | Required for the goal, but not yet implemented or not yet verified. |
| Future | Useful hardening, but outside the current local-platform completion bar unless promoted by product scope. |

## Summary

AgentHero now has a real local durable DAG runtime: app actions queue or run
through shared runtime state, DAG manifests execute through generic node
boundaries, artifacts are hashed and persisted, node provenance is recorded, and
operators can audit runs through AgentHero status/events/logs/metrics without
`ps`, `lsof`, or `pgrep`.

The core local-platform implementation is in place, the latest verification
matrix is green, the branch is consolidated into reviewable commits, and a
final local review pass found only one docs whitespace issue, which was fixed.

The current completion bar is a local trusted-operator AgentHero platform:
Tokio-backed local workers, durable worker leases, policy-enforced local tool
execution, generic verifier invocation, and first-class CLI/HTTP observability.
Remote worker protocols and hardened hostile-code sandboxing are tracked as
next-phase platform hardening unless product scope explicitly promotes them
into this goal.

## Requirement Audit

| Area | AgentHero responsibility | Current evidence | Status | Remaining work |
| --- | --- | --- | --- | --- |
| DAG execution | Parse DAG, schedule nodes, run dependencies, retry/fail/skip. | `crates/dag-runtime/src/lib.rs` validates manifests, node kinds, schemas, approvals, budgets, and policy fields. `crates/dag-executor/src/lib.rs` executes layered DAG nodes and records node reports. `crates/dag-executor/tests/executor.rs` covers dependency layers, retry scheduling, failure propagation, skipped descendants, branches, loops, approval pause, and checkpoint replay. Latest matrix passes. | Implemented | Re-run after further code edits. |
| Concurrency | Tokio tasks, worker pools, backpressure, cancellation. | `crates/dag-executor/src/lib.rs` uses bounded concurrent ordinary-node layers and cancellation tokens. `crates/orchestrator/src/scheduler.rs` uses Tokio tasks for adapter event/log capture, worker lease handling, and cancellation cleanup. `worker_nodes` and `worker_leases` make local worker ownership durable. `platform-smoke` cancellation run `7fd1b928-5cde-4a77-8dfb-122b685fddda` proved durable cleanup events without process inspection. | Implemented for local workers; Future for distributed remote workers | Add loom-style race tests later if scheduler races become likely. |
| Contracts | Input/output schemas, artifact manifests, typed node boundaries. | `DagIo` uses named JSON values plus artifact refs at executor boundary. Tool `input_schema` and `output_schema` are validated before/after handlers. App manifests declare actions, DAGs, observability, tools, roles, and outputs. GrokRxiv/c2rust/formal-proofs keep domain meaning in app code. | Implemented | Keep schema, prompt, manifest, and adapter tests in lockstep during CLI/app cleanup. |
| Artifacts | Store files, logs, proofs, diffs, reports, checkpoints. | `dag_artifacts` and `.agenthero` artifact roots store named refs with `sha256` and size metadata. GrokRxiv stores Lean source/results and review artifacts. C2Rust and formal-proofs store reports and verifier artifacts. App-run logs are file-backed. DAG reports are persisted as replay checkpoints. Latest smoke set reports zero missing artifact hashes for completed and expected-failure runs. | Implemented | Re-run hash checks after further code edits. |
| Tool isolation | Run shell/python/rust/llm/wasm nodes safely. | `GenericToolRunner` executes command-backed generic tools and HTTP nodes with timeout, policy, environment, artifact-root, network, filesystem, approval, and unsafe-runner checks. Tests cover shell/python/rust_binary/llm/lean/haskell/docker/wasm-style policies, nonzero exits, timeout cleanup, missing outputs, and denied policies. `platform-smoke isolation-boundary-smoke` run `841af38e-47e1-4c0e-882e-a397af0abeb5` proves unsafe Docker/WASM host-escape flags are rejected before tool execution and surfaced through app-run state/events/logs. | Implemented for local trusted-operator workloads; Future for hostile-code sandboxing | Hardened multi-tenant sandboxing is next-phase hardening. |
| Provenance | Every node records inputs, outputs, model, prompt hash, command, exit status. | Node reports and durable events include `input_refs`, `output_refs`, `diagnostic_refs`, `model`, `prompt_hash`, `command`, `exit_status`, `manifest_hash`, attempt, status, and duration fields. Scheduler normalizes app/adapter events to the mandatory trace field contract. Latest GrokRxiv, c2rust, formal-proofs, and platform-smoke runs have no missing trace fields. | Implemented | Re-run trace checks after further code edits. |
| Determinism | Replay from artifacts, freeze inputs, compare outputs. | App status exposes determinism summaries: manifest hash, frozen input hash, DAG output hash, node hashes, artifact hash counts, checkpoint availability, replay readiness, and compare readiness. CLI supports `agh app replay` and `agh app compare`. Executor rejects manifest mismatch and artifact drift during checkpoint replay. Current c2rust replay `2c2c1f05-17b8-4b0b-a07c-95f6333f5333` compares work-product equal to source run `e7e74f81-1fce-4170-b83f-39e3b9ba4c2a`. | Implemented for local app runs | Re-run replay/compare after further code edits. |
| Policies | Budget limits, timeout limits, approval gates, network/file permissions. | Runtime schema includes `awaiting_approval`. DAG executor and manifests support budget units, timeouts, approval gates, network policy, filesystem write policy, retry policy, and required/optional behavior. Latest platform smokes cover tool policy, budget exhaustion, approval pause/resume, policy denial, timeout metadata, and isolation requirements. | Implemented | Re-run policy smokes after further code edits. |
| Observability | Event stream, traces, UI/TUI/web hooks, status updates. | CLI exposes `agh app status`, `agh app events`, `agh app logs`, `agh app logs --follow`, `agh app events --follow`. HTTP exposes `/app-runs/:id/events`, `/events/stream`, `/logs`, and `/metrics`. App manifests are required to declare observability and mandatory trace fields. `docs/agenthero-observability-operator-guide.md` documents the operator contract. Latest runs are auditable without `ps`, `lsof`, or `pgrep`. | Implemented for local CLI/HTTP | Hosted dashboard/TUI remains useful next-phase UX; the current bar is satisfied by CLI/HTTP observability plus durable state/log/event storage. |
| Verification routing | Knows how to invoke Lean, Haskell, tests, compilers, but not what theorem means. | Generic verifier/tool kinds route Lean/Haskell/compiler-style commands and record command/output/exit status. GrokRxiv Lean auto-detect/no-lean runs prove AgentHero records verifier evidence while GrokRxiv interprets proof status. `platform-smoke verification-routing-smoke` run `59f8b3f3-63f4-420c-8a4f-5c1de0a00495` records generic Lean and Haskell commands with exit status `0`. Formal-proofs owns certificate meaning. C2Rust owns migration confidence. | Implemented baseline | Add real compiler/Haskell project smokes later if product scope requires broader verifier coverage. |

## Latest Acceptance Evidence

These run IDs are local acceptance evidence from the current branch. They should
be refreshed before final merge.

| App | Command | Run id | Evidence |
| --- | --- | --- | --- |
| GrokRxiv no Lean | `target/debug/agh --json app run grokrxiv review 2606.24837 --type arxiv --no-lean --no-external-actions` | `493c6d0d-d23d-4b4d-a651-a88c32748e83` | `done`; `lean_policy.disabled == true`; Lean result artifact has `skipped == true`; Lean command and exit status are null; 42 events; no missing trace fields; 64 artifacts; zero missing artifact hashes; replay/compare ready. |
| GrokRxiv auto-detect Lean | `target/debug/agh --json app run grokrxiv review 2606.24837 --type arxiv --no-external-actions` | `4697406a-471f-4731-955b-b51b86474edf` | `done`; `lean_policy.auto_detect == true`; Lean command recorded as `["lean","review_loop/lean/GrokRxiv/Proofs.lean"]`; node `exit_status == 0`; proof failure interpretation stays in GrokRxiv artifacts; 42 events; no missing trace fields; 70 artifacts; zero missing artifact hashes. |
| Formal-proofs | `target/debug/agh --json app run formal-proofs theorem-triage --target e677-fin-e255` | `2611e009-0fac-417c-880c-e842ade33945` | `done`; 18 events; no missing trace fields; 12 artifacts; zero missing artifact hashes; replay/compare ready. |
| C2Rust | `target/debug/agh --json app run c2rust migrate agenthero/apps/c2rust/rust/src/lib.rs` | `e7e74f81-1fce-4170-b83f-39e3b9ba4c2a` | `done`; 16 events; no missing trace fields; 12 artifacts; zero missing artifact hashes; replay/compare ready; adapter report events include mandatory trace fields. |
| C2Rust checkpoint replay | `target/debug/agh --json app replay e7e74f81-1fce-4170-b83f-39e3b9ba4c2a`, then `target/debug/agh --json app work --run-id 2c2c1f05-17b8-4b0b-a07c-95f6333f5333`, then `target/debug/agh --json app compare e7e74f81-1fce-4170-b83f-39e3b9ba4c2a 2c2c1f05-17b8-4b0b-a07c-95f6333f5333` | `2c2c1f05-17b8-4b0b-a07c-95f6333f5333` | Replay finished `done`; 16 events; no missing trace fields; same app/action/DAG/manifest; raw identity-sensitive hashes differ; normalized frozen input, normalized DAG output, normalized node outputs, artifacts, and work product match. |
| Platform-smoke cancellation | `target/debug/agh --json app run platform-smoke cancellation-smoke`, then `target/debug/agh --json app cancel 7fd1b928-5cde-4a77-8dfb-122b685fddda --reason final-verification-cancel-smoke` | `7fd1b928-5cde-4a77-8dfb-122b685fddda` | `cancelled`; 12 events; no missing trace fields; recorded `app_run.cancel_observed`, `node.cancelled`, `dag.cancelled`, `app_action.cancelled`, and `app_run.cancel_cleanup_finished` without process inspection. |
| Platform-smoke approval resume | `target/debug/agh --json app run platform-smoke approval-pause-smoke`, then `target/debug/agh app approve-run --json e3527667-b84f-493d-9da7-cf92ec0f6f3f --key approval/human_release`, then `target/debug/agh --json app work --run-id e3527667-b84f-493d-9da7-cf92ec0f6f3f` | `e3527667-b84f-493d-9da7-cf92ec0f6f3f` | Initial run paused at `awaiting_approval`; approval requeued the same app run; resumed worker pass finished `done`; attempt advanced to 2; 22 events; no missing trace fields; logs show `app_run.approved_requeued`, resumed node completion, command, and `exit_status=0`. |
| Platform-smoke isolation boundary | `target/debug/agh --json app run platform-smoke isolation-boundary-smoke`; `target/debug/agh --json app events 841af38e-47e1-4c0e-882e-a397af0abeb5`; `target/debug/agh app logs 841af38e-47e1-4c0e-882e-a397af0abeb5 --tail 80` | `841af38e-47e1-4c0e-882e-a397af0abeb5` | Intentional `failed` state; Docker `--privileged` and WASM `--dir=/` were rejected before spawn; 12 durable events; no missing trace fields; 2 artifacts; zero missing artifact hashes; logs include structured `@@AGENTHERO_EVENT` JSONL records. |
| Platform-smoke tool policy | `target/debug/agh --json app run platform-smoke tool-policy-smoke` | `3ae4798a-c06f-4921-a181-519aa617f744` | `done`; approval-required tool and approval gate completed; 12 events; no missing trace fields; 5 artifacts; zero missing artifact hashes; command, exit status, diagnostics, budget metadata, and artifact hashes recorded. |
| Platform-smoke budget denial | `target/debug/agh --json app run platform-smoke budget-consumption-smoke` | `b2e2eb69-59d2-4066-a47c-c943d22c2942` | Intentional `failed` state; second budgeted node denied because only 1 unit remained; 12 events; no missing trace fields; 6 artifacts; zero missing artifact hashes; policy summary shows one denied node. |
| Platform-smoke verification routing | `target/debug/agh --json app run platform-smoke verification-routing-smoke` | `59f8b3f3-63f4-420c-8a4f-5c1de0a00495` | `done`; generic Lean and Haskell verifier commands recorded with exit status `0`; 12 events; no missing trace fields; 10 artifacts; zero missing artifact hashes. |
| Platform-smoke policy denial | `target/debug/agh --json app run platform-smoke policy-denial-smoke` | `8b8db5a6-9f70-4f4a-b269-7c1e8749f6dc` | Intentional `failed` state; host shell rejected because network/file policy requires isolation; 10 events; no missing trace fields; 1 artifact; zero missing artifact hashes; policy summary shows isolated/network-denied/policy-denied counts. |
| GrokRxiv CLI option validation | `cargo test -p agenthero-orchestrator --test dag_app_registry app_action_args -- --nocapture`; `cargo test -p agenthero-orchestrator --test agenthero_cli_contract every_grokrxiv_manifest_action -- --nocapture`; `target/debug/agh --json app run grokrxiv review --no-external-actions`; `target/debug/agh --json app run grokrxiv review 2606.24837 --type --no-external-actions` | Manifest contract tests and live CLI failures | Generic app action validation now rejects missing required positionals, missing required flags, missing declared flag values, and declared conflicts before queueing. Every GrokRxiv action has a parseable sample command, every documented GrokRxiv flag validates once with a representative value, and live missing-source/missing-`--type` commands fail before adapter execution. |

## Tests Already Passing

Refresh this list after any final code edits:

- `cargo build -p agenthero-orchestrator --bin agh`
- `cargo test -p agenthero-dag-runtime --test manifest -- --nocapture`
- `cargo test -p agenthero-dag-executor -- --nocapture`
- `cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace -- --nocapture`
- `cargo test --manifest-path agenthero/apps/c2rust/Cargo.toml --workspace -- --nocapture`
- `cargo test --manifest-path agenthero/apps/platform-smoke/Cargo.toml --workspace -- --nocapture`
- `pnpm --dir agenthero/apps/formal-proofs test`
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract -- --nocapture`
- `cargo test -p agenthero-orchestrator --test dag_app_registry -- --nocapture`
- `cargo test -p agenthero-orchestrator --test app_runtime_schema -- --nocapture`
- `cargo test -p agenthero-orchestrator --test dag_app_registry app_action_args -- --nocapture`
- `cargo test -p agenthero-orchestrator --test agenthero_cli_contract every_grokrxiv_manifest_action -- --nocapture`
- `cargo test -p agenthero-orchestrator --lib -- --nocapture`
- `cargo test -p agenthero-dag-executor generic_tool_runner_rejects_unsafe_container_and_wasm_flags_without_spawning -- --nocapture`
- `cargo test --manifest-path agenthero/apps/platform-smoke/Cargo.toml --test adapter platform_smoke_adapter_reports_failed_lifecycle_for_isolation_boundary_dag -- --nocapture`
- `cargo fmt --check`
- `cargo check --workspace`
- `git diff --check`

The no-process-inspection scan also passed with no matches:

```bash
rg -n "\bpgrep\b|Command::new\(\"pgrep\"|\bps\b|\blsof\b" \
  crates \
  agenthero/apps/platform-smoke \
  agenthero/apps/c2rust \
  agenthero/apps/formal-proofs \
  agenthero/apps/grokrxiv/rust
```

## Completion Blockers

No blockers remain for the current local trusted-operator AgentHero platform
bar. The final review fix touched documentation only; runtime verification does
not need to be re-run for that whitespace-only docs amendment.

## Boundary Check

The separation rule still holds:

- AgentHero owns mechanics: DAG scheduling, runtime state, contracts, artifacts,
  policies, provenance, determinism, observability, and verifier invocation.
- Apps own meaning: GrokRxiv paper review and Lean-proof significance,
  formal-proofs certificate trust, and C2Rust migration confidence.

If a future change only makes sense for paper review, proof search, or C-to-Rust
semantics, it belongs in the app adapter or app-owned crates. If it remains
useful when GrokRxiv disappears, it belongs in AgentHero.
