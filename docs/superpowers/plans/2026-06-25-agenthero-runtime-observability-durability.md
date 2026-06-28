# AgentHero Runtime Observability And Durability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make AgentHero observability, crash recovery, cancellation, and app-neutral runtime boundaries real platform contracts instead of partially implemented local conveniences.

**Architecture:** Keep AgentHero as the Rust/Tokio DAGOps control plane. The platform owns shared run state, lease fencing, checkpoint resume, trace/event/log contracts, metrics, HTTP monitoring surfaces, and cooperative cancellation; apps emit adapter events and own domain meaning behind process adapters. GrokRxiv moves toward the same thin-adapter shape already used by `c2rust` and `platform-smoke` without adding app dependencies to `crates/orchestrator`.

**Tech Stack:** Rust 1.82, Tokio, `tokio-util::sync::CancellationToken`, `tracing`, `tracing-subscriber`, `tracing-appender`, `metrics`, `metrics-exporter-prometheus`, Axum, Tower, SQLx/Postgres, AgentHero app manifests, durable `app_runs`/`dag_runs`/`dag_run_nodes`/`dag_artifacts`/`dag_events`.

## Global Constraints

- Mandatory trace fields stay exactly: `app_run_id`, `dag_run_id`, `node_id`, `attempt`, `node_kind`, `tool_id`, `manifest_hash`, `artifact_id`, `lease_id`, `status`, `exit_status`, `duration_ms`.
- Every durable event payload, adapter event payload, and app-run log structured event includes every mandatory trace field, using JSON null when the field does not apply.
- `tracing` is the internal logging/span API for the runtime, scheduler, executor, and adapters.
- Durable `dag_events` and app-run logs are the source of truth for CLI, HTTP, TUI, and web monitoring.
- Metrics cover queue depth, node duration, retry count, failure count, lease renewals, artifact bytes, cancellation, and adapter process outcomes.
- HTTP observability uses Axum/Tower routes for status, events, SSE streams, logs, health, metrics, and webhook intake.
- Cancellation crossing scheduler, workers, subprocesses, adapters, and DAG executor uses `tokio-util::sync::CancellationToken`.
- Crash recovery must resume from durable completed node attempts when a checkpoint can be reconstructed; it must not blindly rerun paid LLM nodes after every worker crash.
- Worker leases must include a fencing token so stale workers cannot publish terminal state after a replacement lease has recovered the run.
- Generic platform crates must not encode GrokRxiv-only roles, review ids, paper defaults, or help text.
- GrokRxiv app files stay under `agenthero/apps/grokrxiv/`; shared AgentHero migrations stay under `agenthero/migrations/` and mirrored in `supabase/migrations/`.
- `loom` race tests are not part of this implementation branch. Record exact scheduler/lease/cancellation invariants that should get `loom` coverage after the runtime hardening lands.

---

## Current Baseline

- `crates/agent-runtime/src/app_protocol.rs` already defines `AGENTHERO_EVENT_TRACE_FIELDS` and normalizes adapter event payloads.
- `crates/orchestrator/src/telemetry.rs` already initializes JSON tracing and optional `AGENTHERO_LOG_FILE` output with `tracing-appender`.
- `crates/orchestrator/src/lib.rs` already exposes `/healthz`, `/metrics`, `/app-runs/:id`, `/logs`, `/events`, and `/events/stream`.
- `crates/orchestrator/src/app_runs.rs` already upserts `dag_run_nodes` from live adapter node events and persists terminal reports.
- `crates/orchestrator/src/scheduler.rs` already renews leases, polls for cancellation, records durable events, and passes a watch-channel cancellation signal to adapter processes.
- `crates/dag-executor/src/lib.rs` already supports checkpoint replay and a custom `DagCancellationToken`.
- `agenthero/apps/c2rust/rust/src/main.rs` and `agenthero/apps/platform-smoke/rust/src/main.rs` are thin adapters around `DagExecutor`.
- `agenthero/apps/grokrxiv/rust/src/main.rs` remains a thick adapter/runtime bridge and still routes some public review cases to the legacy GrokRxiv runtime.

## 2026-06-28 GrokRxiv Visibility Gap Audit

This section records the live gaps found while running `agh app run grokrxiv review 2606.25943 --with-lean` and the queued `formalize` app-run. The goal is not to add paper-specific traces. The goal is a platform-level contract where every LLM/API/CLI/subprocess step is observable without guessing, log scraping, `tail`, `wc`, or `pgrep`.

### What Is Visible Today

- App-run level:
  - `agh app status <run_id> --json` shows run state, lease state, latest DAG run, recent node events, live nodes, artifact summaries, and observability commands.
  - `agh app events <run_id> --json` exposes durable event rows.
  - `agh app logs <run_id>` exposes the adapter stderr log.
  - The app manifest advertises `events=true`, `logs=true`, `status=true`, `event_stream=true`, lifecycle events, and required trace fields.
- Adapter/process level:
  - Adapter stderr lines are persisted as `app_log.stderr` events.
  - Formalize inventory, library, Lean-check, and faithfulness stages emit first-class `node.started` / `node.completed` / `node.failed` events with stage-specific artifact refs.
  - Lean proof-author nodes currently emit first-class `node.started` events with target artifact paths and `timeout_secs=null` for unbounded authoring.
- CLI-runner level:
  - The GrokRxiv `CliRunner` writes `command.json`, `agent_status.live.json`, `agent_events.live.jsonl`, `raw_stdout.live.txt`, and `raw_stderr.live.txt` in the prepared role workdir.
  - `agent_input` log lines show `role`, `provider`, `model`, `prompt_chars`, `review_input_bytes`, `schema_bytes`, and `workdir`.
  - `agent_command` log lines show the redacted command shape, backend, model, and timeout policy.
  - Claude runs use stream-json with verbose/partial-message/hook events. Codex runs use `codex exec --json --output-schema`.

### Gaps

- Agent calls are not normalized across the app pipeline.
  - Some LLM steps are first-class `node.*` events, while many review roles, HTML quality, PR cleanup, and verifier actions still appear only as `app_log.stderr`.
  - Operators should be able to query `agent_call.started`, `agent_call.stream`, `agent_call.completed`, and `agent_call.failed` events without parsing log text.
- CLI live files are not consistently durable app artifacts.
  - For target-local Lean workdirs, live files are under the review artifact tree and are inspectable.
  - For temp workdirs such as `/var/folders/.../grokrxiv-review-*`, the live files may disappear after the process exits because the runner owns a tempdir.
  - Every CLI call should attach durable refs for `command.json`, `agent_status.live.json`, `agent_events.live.jsonl`, `raw_stdout.live.txt`, and `raw_stderr.live.txt` to the app-run event stream before the child starts.
- API calls do not have the same observability contract as CLI calls.
  - Direct provider API calls need durable `agent_call.*` events with provider, model, endpoint class, request hash, response hash, retry number, fallback provider/model, latency, token usage, finish reason, and schema validation result.
  - Payload bodies should not be dumped into logs, but prompt/schema/input artifact refs and hashes must be recorded.
- Timeouts are still mixed with control flow.
  - Long coding-agent/artifact-repair roles should not be killed by short default watchdogs.
  - Bounded roles must report timeout policy, elapsed time, last stream activity, last durable event, and resume strategy.
  - A timeout must not silently discard partial stdout/stderr; partial output should become a durable artifact and a resumed attempt should start from that artifact.
- Cancellation is platform-level but child-process behavior is still not fully proven.
  - `agh app cancel <run_id>` exists and updates app-run state.
  - The contract still needs proof that cancellation propagates from app-run lease to adapter process to CLI child process group and then records a terminal `agent_call.cancelled` event.
- The event stream does not yet expose enough "what is the agent doing now" detail.
  - Stream chunks are captured in live files, but the app event stream mostly sees summary lines.
  - Operators should see last stream type, byte counters, provider event type, tool-call names, and last activity timestamp in `agh app status`.
- External side effects need consistent spans.
  - GitHub PR open/update/comment, web revalidation, citation resolver calls, extraction downloads, Lake/Lean commands, and file artifact writes should all emit platform events with the same trace fields.
- Review-loop completeness currently knows some async Lean artifacts are deferred, but the contract should be explicit.
  - `review --with-lean` queues formalization after the review/PR path.
  - The synchronous review-loop bundle must mark async Lean artifacts as `pending_async_formalize` or `deferred_to_formalize`; it must never fail because `formalize` has not produced `review_loop/lean/env/env_result.json` yet.

### Typed-IR Status

Typed-IR was supposed to be removed from the paper-to-Lean MVP path as a gate and as a source of truth.

Resolved direction: the default GrokRxiv formalization path must not run typed-IR or use typed-IR/semantic-IR/proof-obligation artifacts as the source of truth for Lean authoring. Historical runs can contain older typed-IR artifacts, but new default `formalize` runs must route through theorem inventory, source context, paper-local library authoring, per-target `Proofs.lean`, Lean diagnostics, and faithfulness review.

Required direction:

- The Lean MVP source path should be:
  - `theorem_inventory.json` / exact `source_tex`
  - nearby source context and referenced definitions
  - LLM-authored paper-local Lean library
  - per-target `GrokRxiv/Proofs.lean`
  - Lean/Lake diagnostic result
  - source-faithfulness review
- Typed-IR/semantic-IR must not be a required gate before `Proofs.lean` exists.
- Historical typed-IR artifacts may be ignored or stripped for compatibility, but they must not be reused from stale cache as authoritative theorem data and must never block source-to-Lean authoring.
- Acceptance for removing typed-IR from the Lean MVP path:
  - A default `formalize` run has no `formalize_typed_ir*` nodes.
  - Per-target proof-author packets are built from theorem inventory/source context/paper-local library, not from typed-IR or deterministic semantic mapping.
  - Every selected target writes `review_loop/lean/targets/<target>/GrokRxiv/Proofs.lean` before compile diagnostics.
  - Failures report compiler errors, missing paper-local library objects, source ambiguity, faithfulness failure, or incomplete proof; not "typed-IR unavailable" or stale graph reuse.

## File And Surface Map

- Modify: `Cargo.toml`
  - Add workspace dependencies for `tokio-util`, `metrics`, and `metrics-exporter-prometheus`.
- Modify: `crates/agent-runtime/src/app_protocol.rs`
  - Keep protocol constants and expose reusable trace helpers without app-specific policy.
- Modify: `crates/agent-runtime/src/lib.rs`
  - Re-export trace helpers.
- Modify: `crates/dag-executor/src/lib.rs`
  - Replace custom cancellation token with `tokio_util::sync::CancellationToken`.
  - Emit metrics and span fields around node execution.
- Modify: `crates/dag-executor/Cargo.toml`
  - Add `tokio-util` and `metrics`.
- Modify: `crates/orchestrator/src/app_runs.rs`
  - Add lease fencing, reconstructed checkpoint helpers, stale-lease guards, and durable metric source helpers.
- Modify: `crates/orchestrator/src/scheduler.rs`
  - Use `CancellationToken`, enforce lease fencing, pass reconstructed checkpoints on recovery, and emit metrics.
- Modify: `crates/orchestrator/src/dag_apps.rs`
  - Pass cancellation tokens to adapter processes and include fencing identity in adapter requests.
- Modify: `crates/orchestrator/src/lib.rs`
  - Replace hand-only `/metrics` rendering with a Prometheus exporter handle plus SQL-derived gauges.
  - Add status/health/webhook route tests.
- Modify: `crates/orchestrator/src/serve.rs`
  - Install Tower trace layers and graceful cancellation.
- Modify: `crates/orchestrator/src/config.rs`
  - Remove `GROKRXIV_BIND` fallback from generic platform config.
- Modify: `crates/orchestrator/src/cli.rs`
  - Remove GrokRxiv-only review columns and hardcoded GrokRxiv help from generic app-run output.
- Modify: `crates/dag-runtime/src/lib.rs`
  - Replace closed `AgentKind` enum with a validated string/newtype while preserving manifest validation.
- Create: `agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql`
- Create: `supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql`
- Create: `crates/orchestrator/tests/runtime_hardening_contract.rs`
- Modify: `crates/orchestrator/tests/app_runtime_schema.rs`
- Modify: `crates/orchestrator/tests/dag_app_registry.rs`
- Modify: `crates/orchestrator/tests/agenthero_cli_contract.rs`
- Modify: `crates/dag-executor/tests/executor.rs`
- Modify: `agenthero/apps/grokrxiv/rust/src/main.rs`
  - Remove one routed legacy review lane at a time, starting with arXiv review and formalize paths.
- Modify: `agenthero/apps/grokrxiv/rust/tests/adapter.rs`
  - Prove GrokRxiv adapter lifecycle and trace fields match thin adapters for migrated lanes.
- Modify: `docs/agenthero-observability-operator-guide.md`
- Modify: `docs/agenthero-platform-completion-audit.md`

---

### Task 1: Add Runtime Hardening Contract Tests

**Files:**
- Create: `crates/orchestrator/tests/runtime_hardening_contract.rs`
- Modify: `crates/orchestrator/tests/app_runtime_schema.rs`
- Modify: `crates/orchestrator/tests/dag_app_registry.rs`

**Interfaces:**
- Consumes: current migrations, registered app descriptors, trace contract constants.
- Produces: failing tests that define the milestone before implementation changes begin.

- [ ] **Step 1: Write the failing migration/schema contract test**

Create `crates/orchestrator/tests/runtime_hardening_contract.rs`:

```rust
fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crate lives under crates/orchestrator")
        .to_path_buf()
}

fn migration_sql() -> String {
    let root = repo_root();
    std::fs::read_to_string(
        root.join("agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql"),
    )
    .expect("runtime hardening migration exists")
}

#[test]
fn runtime_hardening_migration_adds_fencing_and_resume_columns() {
    let sql = migration_sql();
    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");

    for required in [
        "alter table worker_leases add column if not exists fencing_token bigint",
        "alter table worker_leases add column if not exists adapter_submission_key text",
        "alter table app_runs add column if not exists current_dag_run_id uuid",
        "alter table app_runs add column if not exists resume_checkpoint jsonb",
        "create unique index if not exists worker_leases_one_active_app_run_lease_uidx",
        "create unique index if not exists worker_leases_one_active_node_lease_uidx",
    ] {
        assert!(
            compact.contains(required),
            "runtime hardening migration must contain `{required}`"
        );
    }
}

#[test]
fn runtime_hardening_migration_is_mirrored_to_supabase_view() {
    let root = repo_root();
    let platform = std::fs::read_to_string(
        root.join("agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql"),
    )
    .expect("platform migration exists");
    let supabase = std::fs::read_to_string(
        root.join("supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql"),
    )
    .expect("supabase migration exists");

    assert_eq!(platform, supabase);
}
```

- [ ] **Step 2: Write the failing app-neutrality guard**

Append this test to `crates/orchestrator/tests/runtime_hardening_contract.rs`:

```rust
#[test]
fn generic_platform_sources_do_not_reference_grokrxiv_domain_terms() {
    let root = repo_root();
    let generic_files = [
        "crates/orchestrator/src/config.rs",
        "crates/orchestrator/src/cli.rs",
        "crates/agent-runtime/src/types.rs",
        "crates/dag-runtime/src/lib.rs",
    ];
    let forbidden = [
        "GROKRXIV_BIND",
        "review_id",
        "PaperLatex",
        "TypeTheoryValidator",
        "Renderer",
        "Extractor",
        "grokrxiv",
    ];

    for file in generic_files {
        let text = std::fs::read_to_string(root.join(file)).expect("source file is readable");
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{file} must not contain app-specific term `{needle}`"
            );
        }
    }
}
```

- [ ] **Step 3: Run the contract tests and capture the expected failures**

Run:

```bash
cargo test -p agenthero-orchestrator --test runtime_hardening_contract -- --nocapture
```

Expected: FAIL because the new migration does not exist and generic sources still contain the app-specific terms listed in the test.

- [ ] **Step 4: Commit the failing contract test**

Run:

```bash
git add crates/orchestrator/tests/runtime_hardening_contract.rs
git commit -m "test: define AgentHero runtime hardening contract"
```

Expected: commit succeeds with intentionally failing tests documented by the next tasks.

---

### Task 2: Add Lease Fencing And Resume Schema

**Files:**
- Create: `agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql`
- Create: `supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql`
- Modify: `crates/orchestrator/tests/app_runtime_schema.rs`

**Interfaces:**
- Consumes: `worker_leases`, `app_runs`, `dag_runs`, `dag_run_nodes`.
- Produces: schema support for one active lease, stale-worker fencing, and recovery checkpoint attachment.

- [ ] **Step 1: Add the platform migration**

Create `agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql`:

```sql
-- Add lease fencing and app-run resume checkpoint support.

create sequence if not exists worker_lease_fencing_token_seq;

alter table worker_leases
  add column if not exists fencing_token bigint;

update worker_leases
   set fencing_token = nextval('worker_lease_fencing_token_seq')
 where fencing_token is null;

alter table worker_leases
  alter column fencing_token set default nextval('worker_lease_fencing_token_seq'),
  alter column fencing_token set not null;

alter table worker_leases
  add column if not exists adapter_submission_key text;

alter table app_runs
  add column if not exists current_dag_run_id uuid references dag_runs(id) on delete set null,
  add column if not exists resume_checkpoint jsonb,
  add column if not exists resume_checkpoint_created_at timestamptz;

create unique index if not exists worker_leases_one_active_app_run_lease_uidx
  on worker_leases(app_run_id)
  where state = 'leased' and app_run_id is not null;

create unique index if not exists worker_leases_one_active_node_lease_uidx
  on worker_leases(node_run_id)
  where state = 'leased' and node_run_id is not null;

create unique index if not exists worker_leases_adapter_submission_uidx
  on worker_leases(app_run_id, adapter_submission_key)
  where adapter_submission_key is not null;

create index if not exists app_runs_current_dag_run_idx
  on app_runs(current_dag_run_id);
```

- [ ] **Step 2: Mirror the migration into the combined Supabase view**

Run:

```bash
cp agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql \
  supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql
```

Expected: the files are byte-identical.

- [ ] **Step 3: Extend the runtime schema test**

Append to `crates/orchestrator/tests/app_runtime_schema.rs`:

```rust
#[test]
fn runtime_hardening_forward_migration_is_mirrored_and_contractual() {
    let root = repo_root();
    let sql = std::fs::read_to_string(
        root.join("agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql"),
    )
    .expect("platform runtime hardening migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql"),
    )
    .expect("supabase runtime hardening migration should exist");
    assert_eq!(sql, supabase_sql);

    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    for required in [
        "worker_lease_fencing_token_seq",
        "fencing_token bigint",
        "adapter_submission_key text",
        "current_dag_run_id uuid references dag_runs(id)",
        "resume_checkpoint jsonb",
        "resume_checkpoint_created_at timestamptz",
        "worker_leases_one_active_app_run_lease_uidx",
        "worker_leases_one_active_node_lease_uidx",
    ] {
        assert!(compact.contains(required), "missing `{required}`");
    }
}
```

- [ ] **Step 4: Run schema tests**

Run:

```bash
cargo test -p agenthero-orchestrator --test app_runtime_schema -- --nocapture
cargo test -p agenthero-orchestrator --test runtime_hardening_contract -- --nocapture
```

Expected: schema and migration assertions pass. The app-neutrality assertion remains failing until Task 8.

- [ ] **Step 5: Commit the schema change**

Run:

```bash
git add agenthero/migrations/20260625000001_runtime_lease_fencing_resume.sql \
  supabase/migrations/20260625000001_runtime_lease_fencing_resume.sql \
  crates/orchestrator/tests/app_runtime_schema.rs
git commit -m "feat: add runtime lease fencing schema"
```

Expected: commit succeeds.

---

### Task 3: Reconstruct Resume Checkpoints From Durable Runtime State

**Files:**
- Modify: `crates/orchestrator/src/app_runs.rs`
- Modify: `crates/orchestrator/src/scheduler.rs`
- Modify: `crates/orchestrator/tests/runtime_hardening_contract.rs`

**Interfaces:**
- Consumes: persisted `dag_runs`, `dag_run_nodes`, `dag_artifacts`, event-derived node output payloads.
- Produces: `StoredAppRunInput.checkpoint` on recovered retries when completed nodes can be replayed.

- [ ] **Step 1: Add a unit test for checkpoint reconstruction**

Append to `crates/orchestrator/src/app_runs.rs` tests:

```rust
#[test]
fn reconstructed_checkpoint_preserves_completed_node_outputs() {
    let dag_run_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa").unwrap();
    let node = StoredNodeCheckpoint {
        node_id: "expensive_llm".to_string(),
        node_kind: "agent".to_string(),
        state: "ok".to_string(),
        attempt: 1,
        output: serde_json::json!({
            "outputs": {
                "values": { "review": { "verdict": "accept" } },
                "artifacts": {}
            }
        }),
        output_refs: serde_json::json!({}),
        diagnostic_refs: serde_json::json!({}),
        latency_ms: Some(42),
    };

    let report = checkpoint_report_from_stored_nodes(
        "paper-review",
        1,
        "manifest-hash",
        dag_run_id,
        serde_json::json!({"values": {}, "artifacts": {}}),
        vec![node],
    )
    .expect("checkpoint reconstruction succeeds");

    assert_eq!(report.dag_type, "paper-review");
    assert_eq!(report.nodes.len(), 1);
    assert_eq!(report.nodes[0].node_id, "expensive_llm");
    assert_eq!(report.nodes[0].status, agenthero_dag_runtime::DagNodeStatus::Ok);
    assert_eq!(report.outputs.values["review"]["verdict"], "accept");
}
```

Expected: FAIL because `StoredNodeCheckpoint` and `checkpoint_report_from_stored_nodes` do not exist.

- [ ] **Step 2: Add checkpoint reconstruction types**

Add near the existing replay helpers in `crates/orchestrator/src/app_runs.rs`:

```rust
#[derive(Debug, Clone)]
struct StoredNodeCheckpoint {
    node_id: String,
    node_kind: String,
    state: String,
    attempt: i32,
    output: serde_json::Value,
    output_refs: serde_json::Value,
    diagnostic_refs: serde_json::Value,
    latency_ms: Option<i32>,
}
```

- [ ] **Step 3: Add reconstruction conversion**

Add in `crates/orchestrator/src/app_runs.rs`:

```rust
fn checkpoint_report_from_stored_nodes(
    dag_type: &str,
    manifest_version: i32,
    manifest_hash: &str,
    dag_run_id: Uuid,
    input: serde_json::Value,
    nodes: Vec<StoredNodeCheckpoint>,
) -> anyhow::Result<DagExecutionReport> {
    let mut outputs = serde_json::from_value::<DagIo>(input.clone()).unwrap_or_default();
    let mut reports = Vec::new();

    for node in nodes {
        let status = match node.state.as_str() {
            "ok" => DagNodeStatus::Ok,
            "degraded" => DagNodeStatus::Degraded,
            "skipped" => DagNodeStatus::Skipped,
            "failed" => DagNodeStatus::Failed,
            _ => continue,
        };
        if let Some(produced) = node.output.get("outputs").cloned() {
            if let Ok(produced_io) = serde_json::from_value::<DagIo>(produced) {
                outputs.values.extend(produced_io.values);
                outputs.artifacts.extend(produced_io.artifacts);
            }
        }
        reports.push(agenthero_dag_runtime::DagNodeReport {
            node_id: node.node_id,
            kind: node.node_kind,
            status,
            attempt: u32::try_from(node.attempt.max(1)).unwrap_or(1),
            executor: None,
            role: None,
            tool: None,
            child_dag_type: None,
            required: true,
            warning: None,
            error: None,
            latency_ms: node.latency_ms.map(|value| value.max(0) as u64).unwrap_or(0),
            input_refs: Default::default(),
            output_refs: serde_json::from_value(node.output_refs).unwrap_or_default(),
            diagnostic_refs: serde_json::from_value(node.diagnostic_refs).unwrap_or_default(),
            policy: Default::default(),
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: Default::default(),
        });
    }

    Ok(DagExecutionReport {
        dag_type: dag_type.to_string(),
        manifest_version,
        manifest_hash: manifest_hash.to_string(),
        status: DagNodeStatus::Degraded,
        input: serde_json::from_value(input).unwrap_or_default(),
        nodes: reports,
        outputs,
        events: vec![agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "dag.resume_checkpoint_reconstructed".to_string(),
            node_id: None,
            message: Some(format!("resume checkpoint reconstructed for {dag_run_id}")),
            payload: Default::default(),
        }],
    })
}
```

- [ ] **Step 4: Add DB loader and requeue attachment**

Add the public helpers in `crates/orchestrator/src/app_runs.rs`:

```rust
pub async fn reconstruct_resume_checkpoint(
    pool: &PgPool,
    app_run_id: Uuid,
    dag_run_id: Uuid,
) -> anyhow::Result<Option<DagExecutionReport>> {
    let Some(dag_row) = sqlx::query(
        "select dag_type, manifest_version, manifest_hash, input \
         from dag_runs \
         where id = $1 and app_run_id = $2",
    )
    .bind(dag_run_id)
    .bind(app_run_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let manifest_hash = match dag_row.get::<Option<String>, _>("manifest_hash") {
        Some(value) if !value.is_empty() => value,
        _ => return Ok(None),
    };
    let dag_type: String = dag_row.get("dag_type");
    let manifest_version = dag_row
        .get::<Option<i32>, _>("manifest_version")
        .unwrap_or(1);
    let input: serde_json::Value = dag_row.get("input");

    let node_rows = sqlx::query(
        "select node_id, node_kind, state, attempt, output, output_refs, \
                diagnostic_refs, latency_ms \
         from dag_run_nodes \
         where dag_run_id = $1 \
           and state in ('ok', 'degraded', 'skipped') \
         order by created_at asc, node_id asc, attempt asc",
    )
    .bind(dag_run_id)
    .fetch_all(pool)
    .await?;

    if node_rows.is_empty() {
        return Ok(None);
    }

    let nodes = node_rows
        .into_iter()
        .map(|row| StoredNodeCheckpoint {
            node_id: row.get("node_id"),
            node_kind: row.get("node_kind"),
            state: row.get("state"),
            attempt: row.get("attempt"),
            output: row.get("output"),
            output_refs: row.get("output_refs"),
            diagnostic_refs: row.get("diagnostic_refs"),
            latency_ms: row.get("latency_ms"),
        })
        .collect();

    checkpoint_report_from_stored_nodes(
        &dag_type,
        manifest_version,
        &manifest_hash,
        dag_run_id,
        input,
        nodes,
    )
    .map(Some)
}

pub async fn attach_resume_checkpoint(
    tx: &mut Transaction<'_, Postgres>,
    app_run_id: Uuid,
    checkpoint: &DagExecutionReport,
) -> anyhow::Result<()> {
    let row = sqlx::query("select input from app_runs where id = $1 for update")
        .bind(app_run_id)
        .fetch_one(&mut **tx)
        .await?;
    let mut input: StoredAppRunInput = serde_json::from_value(row.get("input"))?;
    input.checkpoint = Some(checkpoint.clone());
    let input_value = serde_json::to_value(&input)?;
    let checkpoint_value = serde_json::to_value(checkpoint)?;
    sqlx::query(
        "update app_runs \
         set input = $2, resume_checkpoint = $3, \
             resume_checkpoint_created_at = now(), updated_at = now() \
         where id = $1",
    )
    .bind(app_run_id)
    .bind(input_value)
    .bind(checkpoint_value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
```

- [ ] **Step 5: Use the helper in expired lease recovery**

Modify `recover_expired_leases` so the requeue path reconstructs a checkpoint before setting `app_runs.state = 'queued'`. If checkpoint reconstruction fails, emit `app_run.resume_checkpoint_failed` and continue with the existing retry behavior only when no terminal node attempts exist. If terminal node attempts exist and reconstruction fails, mark `system_failed` with `error_code = 'resume_checkpoint_failed'`.

- [ ] **Step 6: Run the focused tests**

Run:

```bash
cargo test -p agenthero-orchestrator --lib reconstructed_checkpoint_preserves_completed_node_outputs -- --nocapture
cargo test -p agenthero-orchestrator --lib expired_lease -- --nocapture
```

Expected: tests pass and expired lease tests cover requeue, system failure, and checkpoint attachment.

- [ ] **Step 7: Commit checkpoint recovery**

Run:

```bash
git add crates/orchestrator/src/app_runs.rs crates/orchestrator/src/scheduler.rs
git commit -m "feat: resume app runs from durable DAG checkpoints"
```

Expected: commit succeeds.

---

### Task 4: Enforce Lease Fencing In Scheduler Completion Paths

**Files:**
- Modify: `crates/orchestrator/src/app_runs.rs`
- Modify: `crates/orchestrator/src/scheduler.rs`
- Modify: `crates/orchestrator/tests/runtime_hardening_contract.rs`

**Interfaces:**
- Consumes: `ClaimedAppRun.lease_id`, `worker_leases.fencing_token`, `app_runs.current_dag_run_id`.
- Produces: stale worker attempts cannot update terminal state, release replacement leases, or publish late adapter results.

- [ ] **Step 1: Add fencing token to claimed runs**

Modify `ClaimedAppRun` in `crates/orchestrator/src/app_runs.rs`:

```rust
pub struct ClaimedAppRun {
    pub id: Uuid,
    pub worker_id: Uuid,
    pub app_id: String,
    pub action_id: String,
    pub input: StoredAppRunInput,
    pub dag_run_id: Uuid,
    pub lease_id: Uuid,
    pub fencing_token: i64,
    pub attempt: i32,
}
```

- [ ] **Step 2: Return fencing token from claim SQL**

Update `claim_next` and `claim_run` so the `insert into worker_leases ... returning id, fencing_token` result is captured and `app_runs.current_dag_run_id` is set to the new DAG id in the same transaction.

- [ ] **Step 3: Add fenced completion helpers**

Change completion calls to accept `AppRunRuntimeIdentity` with `fencing_token`:

```rust
pub struct AppRunRuntimeIdentity {
    pub dag_run_id: Uuid,
    pub lease_id: Uuid,
    pub fencing_token: i64,
}
```

Update terminal SQL predicates from:

```sql
where id = $1 and state = 'running'
```

to:

```sql
where id = $1
  and state = 'running'
  and exists (
    select 1
      from worker_leases wl
     where wl.id = $2
       and wl.app_run_id = app_runs.id
       and wl.state = 'leased'
       and wl.fencing_token = $3
  )
```

- [ ] **Step 4: Add stale completion unit test**

Add to `crates/orchestrator/src/app_runs.rs` tests:

```rust
#[test]
fn fenced_runtime_identity_carries_lease_and_token() {
    let identity = AppRunRuntimeIdentity {
        dag_run_id: uuid::Uuid::nil(),
        lease_id: uuid::Uuid::nil(),
        fencing_token: 7,
    };
    let payload = runtime_event_payload(
        Some(identity),
        None,
        serde_json::json!({"state": "done"}),
    );

    assert_eq!(payload["lease_id"], uuid::Uuid::nil().to_string());
    assert_eq!(payload["fencing_token"], 7);
}
```

- [ ] **Step 5: Emit stale-worker events**

When a fenced update affects zero rows, insert a durable `app_run.stale_worker_ignored` event only if the app run still exists. Include `lease_id`, `fencing_token`, `dag_run_id`, `status: "ignored"`, and all mandatory trace fields.

- [ ] **Step 6: Run scheduler/app-run tests**

Run:

```bash
cargo test -p agenthero-orchestrator --lib fencing -- --nocapture
cargo test -p agenthero-orchestrator --lib scheduler -- --nocapture
```

Expected: fenced identity tests pass and stale completion is observable without changing the replacement run.

- [ ] **Step 7: Commit fencing behavior**

Run:

```bash
git add crates/orchestrator/src/app_runs.rs crates/orchestrator/src/scheduler.rs
git commit -m "feat: fence stale AgentHero worker completions"
```

Expected: commit succeeds.

---

### Task 5: Replace Custom Cancellation With `tokio-util::CancellationToken`

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/dag-executor/Cargo.toml`
- Modify: `crates/orchestrator/Cargo.toml`
- Modify: `crates/dag-executor/src/lib.rs`
- Modify: `crates/orchestrator/src/scheduler.rs`
- Modify: `crates/orchestrator/src/dag_apps.rs`
- Modify: `crates/dag-executor/tests/executor.rs`

**Interfaces:**
- Consumes: operator app-run cancellation, scheduler worker cancellation, adapter process cancellation, executor cancellation.
- Produces: one cancellation primitive across scheduler, subprocesses, and DAG executor.

- [ ] **Step 1: Add the dependency**

Modify workspace `Cargo.toml`:

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

Add to `crates/dag-executor/Cargo.toml` and `crates/orchestrator/Cargo.toml`:

```toml
tokio-util = { workspace = true }
```

- [ ] **Step 2: Replace `DagCancellationToken` internals**

In `crates/dag-executor/src/lib.rs`, replace the custom `Arc<AtomicBool>` token with:

```rust
pub type DagCancellationToken = tokio_util::sync::CancellationToken;
```

Update executor cancellation checks:

```rust
fn is_cancelled(&self) -> bool {
    self.cancellation_token
        .as_ref()
        .map(tokio_util::sync::CancellationToken::is_cancelled)
        .unwrap_or(false)
}
```

- [ ] **Step 3: Update async cancellation waits in adapter process runner**

Change `run_adapter_process` and callers in `crates/orchestrator/src/dag_apps.rs` from `Option<watch::Receiver<bool>>` to `Option<CancellationToken>`.

Replace `wait_for_adapter_cancellation` with:

```rust
async fn wait_for_adapter_cancellation(cancellation: Option<CancellationToken>) {
    let Some(cancellation) = cancellation else {
        std::future::pending::<()>().await;
        return;
    };
    cancellation.cancelled().await;
}
```

- [ ] **Step 4: Update scheduler cancellation**

In `crates/orchestrator/src/scheduler.rs`, replace:

```rust
let (adapter_cancel_tx, adapter_cancel_rx) = watch::channel(false);
```

with:

```rust
let adapter_cancel = tokio_util::sync::CancellationToken::new();
let adapter_cancel_for_runner = adapter_cancel.clone();
```

When cancellation is observed:

```rust
adapter_cancel.cancel();
cancellation_requested = true;
```

Pass `Some(adapter_cancel_for_runner)` into the app action call.

- [ ] **Step 5: Update executor tests**

Replace test construction:

```rust
let token = DagCancellationToken::new();
token.cancel();
```

with the same calls against the re-exported `tokio_util` token. Existing assertions for skipped unstarted nodes and `dag.cancelled` events must remain unchanged.

- [ ] **Step 6: Run cancellation tests**

Run:

```bash
cargo test -p agenthero-dag-executor cancellation -- --nocapture
cargo test -p agenthero-orchestrator --lib cancellation -- --nocapture
cargo test -p agenthero-orchestrator --test dag_app_registry cancellation -- --nocapture
```

Expected: cancellation tests pass and adapter process cancellation still kills descendants before late side effects.

- [ ] **Step 7: Commit cancellation primitive migration**

Run:

```bash
git add Cargo.toml Cargo.lock crates/dag-executor/Cargo.toml crates/orchestrator/Cargo.toml \
  crates/dag-executor/src/lib.rs crates/dag-executor/tests/executor.rs \
  crates/orchestrator/src/dag_apps.rs crates/orchestrator/src/scheduler.rs
git commit -m "refactor: use tokio-util cancellation tokens"
```

Expected: commit succeeds.

---

### Task 6: Promote Metrics To A Prometheus Exporter

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/orchestrator/Cargo.toml`
- Modify: `crates/dag-executor/Cargo.toml`
- Modify: `crates/orchestrator/src/lib.rs`
- Modify: `crates/orchestrator/src/scheduler.rs`
- Modify: `crates/dag-executor/src/lib.rs`
- Modify: `docs/agenthero-observability-operator-guide.md`

**Interfaces:**
- Consumes: scheduler events, durable SQL summaries, executor node execution.
- Produces: `/metrics` output with counters, gauges, and histogram families for runtime operations.

- [ ] **Step 1: Add metrics dependencies**

Modify workspace `Cargo.toml`:

```toml
metrics = "0.24"
metrics-exporter-prometheus = { version = "0.16", default-features = false }
```

Add `metrics = { workspace = true }` to `crates/dag-executor/Cargo.toml`.

Add to `crates/orchestrator/Cargo.toml`:

```toml
metrics = { workspace = true }
metrics-exporter-prometheus = { workspace = true }
```

- [ ] **Step 2: Add metrics handle setup**

In `crates/orchestrator/src/lib.rs`, add:

```rust
#[derive(Clone, Default)]
pub struct PlatformMetricsHandle {
    recorder: Option<metrics_exporter_prometheus::PrometheusHandle>,
}

impl PlatformMetricsHandle {
    pub fn install() -> Self {
        match metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder() {
            Ok(handle) => Self {
                recorder: Some(handle),
            },
            Err(_) => Self { recorder: None },
        }
    }

    pub fn render(&self) -> String {
        self.recorder
            .as_ref()
            .map(metrics_exporter_prometheus::PrometheusHandle::render)
            .unwrap_or_default()
    }
}
```

Extend `PlatformState` with `pub metrics: PlatformMetricsHandle`.

- [ ] **Step 3: Keep SQL-derived gauges and append recorder output**

Modify `render_platform_metrics` so it first renders durable SQL gauges and then appends `state.metrics.render()`. Keep existing metric names stable. Add recorder-backed names:

```text
agenthero_scheduler_lease_renewals_total
agenthero_scheduler_lease_recoveries_total
agenthero_scheduler_failures_total
agenthero_adapter_process_runs_total
agenthero_adapter_process_duration_seconds
agenthero_dag_node_duration_seconds
agenthero_dag_node_retries_total
agenthero_dag_artifact_bytes_total
```

- [ ] **Step 4: Emit scheduler metrics**

In `crates/orchestrator/src/scheduler.rs`, increment counters around lease renewals, recoveries, adapter outcomes, cancellations, and stale completions:

```rust
metrics::counter!(
    "agenthero_scheduler_lease_renewals_total",
    "app" => run.app_id.clone(),
    "action" => run.action_id.clone(),
).increment(1);
```

Use labels `app`, `action`, `dag_type`, `status`, and `reason` where each label has bounded values.

- [ ] **Step 5: Emit executor node histograms**

In `crates/dag-executor/src/lib.rs`, record node duration after each attempt:

```rust
metrics::histogram!(
    "agenthero_dag_node_duration_seconds",
    "dag_type" => manifest.id.clone(),
    "node_id" => node.id.clone(),
    "node_kind" => node.kind.to_string(),
    "status" => result_status.to_string(),
)
.record(latency_ms as f64 / 1000.0);
```

Increment `agenthero_dag_node_retries_total` when `retry_scheduled_event` is emitted.

- [ ] **Step 6: Add metrics formatting tests**

Add to `crates/orchestrator/src/lib.rs` tests:

```rust
#[test]
fn platform_metrics_document_prometheus_exporter_names() {
    let mut metrics = PlatformMetrics::default();
    metrics.database_configured = true;
    let text = format_platform_metrics(&metrics);

    assert!(text.contains("agenthero_app_runs"));
    assert!(text.contains("agenthero_dag_runs"));
    assert!(text.contains("agenthero_dag_artifact_bytes_total"));
}
```

Add a unit-level assertion that `PlatformMetricsHandle::default().render()` is empty instead of panicking.

- [ ] **Step 7: Run metrics tests**

Run:

```bash
cargo test -p agenthero-orchestrator --lib platform_metrics -- --nocapture
cargo test -p agenthero-dag-executor --lib metrics -- --nocapture
cargo check --workspace
```

Expected: tests and workspace check pass.

- [ ] **Step 8: Commit metrics exporter**

Run:

```bash
git add Cargo.toml Cargo.lock crates/orchestrator/Cargo.toml crates/dag-executor/Cargo.toml \
  crates/orchestrator/src/lib.rs crates/orchestrator/src/scheduler.rs \
  crates/dag-executor/src/lib.rs docs/agenthero-observability-operator-guide.md
git commit -m "feat: expose AgentHero Prometheus runtime metrics"
```

Expected: commit succeeds.

---

### Task 7: Harden Axum/Tower Observability API

**Files:**
- Modify: `crates/orchestrator/src/lib.rs`
- Modify: `crates/orchestrator/src/serve.rs`
- Modify: `crates/orchestrator/tests/dag_app_registry.rs`
- Modify: `docs/agenthero-observability-operator-guide.md`

**Interfaces:**
- Consumes: `PlatformState`, durable app-run repository, service token auth.
- Produces: documented HTTP routes for status, events, SSE, logs, health, metrics, and webhooks with Tower tracing.

- [ ] **Step 1: Add explicit route aliases**

Keep existing routes and add aliases:

```rust
.route("/status", get(platform_status))
.route("/health", get(platform_health))
.route("/webhooks/:app/:name", post(app_webhook))
```

`/health` returns JSON:

```json
{"ok":true,"database_configured":true}
```

- [ ] **Step 2: Add Tower trace and request body layers**

In `router_with_state`, add:

```rust
.layer(tower_http::trace::TraceLayer::new_for_http())
.layer(tower_http::limit::RequestBodyLimitLayer::new(1024 * 1024))
```

- [ ] **Step 3: Add webhook auth contract**

Implement `app_webhook` so it requires the same `AGENTHERO_SERVICE_TOKEN` bearer token used by write routes. Return:

```json
{"accepted":true,"app":"grokrxiv","webhook":"billing"}
```

when authenticated. Do not route to app-specific code in this task; record the event as `app_webhook.received` when a database pool is configured.

- [ ] **Step 4: Add router tests**

Add tests that call `router_with_state` through `tower::ServiceExt`:

```rust
#[tokio::test]
async fn health_route_returns_json_contract() {
    let app = agenthero_orchestrator::router_with_state(agenthero_orchestrator::PlatformState::default());
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
```

- [ ] **Step 5: Run route tests**

Run:

```bash
cargo test -p agenthero-orchestrator --lib health_route -- --nocapture
cargo test -p agenthero-orchestrator --lib webhook -- --nocapture
```

Expected: route tests pass and existing `/healthz` remains available.

- [ ] **Step 6: Commit API hardening**

Run:

```bash
git add crates/orchestrator/src/lib.rs crates/orchestrator/src/serve.rs \
  crates/orchestrator/tests/dag_app_registry.rs docs/agenthero-observability-operator-guide.md
git commit -m "feat: harden AgentHero observability API routes"
```

Expected: commit succeeds.

---

### Task 8: Remove App-Specific Vocabulary From Generic Platform Crates

**Files:**
- Modify: `crates/dag-runtime/src/lib.rs`
- Modify: `crates/agent-runtime/src/types.rs`
- Modify: `crates/orchestrator/src/config.rs`
- Modify: `crates/orchestrator/src/cli.rs`
- Modify: `crates/dag-runtime/tests/manifest.rs`
- Modify: `crates/orchestrator/tests/agenthero_cli_contract.rs`
- Modify: `crates/orchestrator/tests/runtime_hardening_contract.rs`

**Interfaces:**
- Consumes: app manifests and role YAML.
- Produces: string-based app-owned role identity and generic app-run presentation.

- [ ] **Step 1: Replace `AgentKind` with a validated newtype**

In `crates/dag-runtime/src/lib.rs`, replace the closed enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentKind(String);
```

Add constructor validation:

```rust
impl AgentKind {
    pub fn new(value: impl Into<String>) -> Result<Self, DagError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || !value
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
        {
            return Err(DagError::InvalidAgentKind(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

Add the matching error variant:

```rust
#[error("agent kind `{0}` must be a lowercase slug")]
InvalidAgentKind(String),
```

Update manifest validation to call `AgentKind::new(role.kind.as_str().to_string())` for every accepted kind and role kind.

- [ ] **Step 2: Move revision target defaults out of `agent-runtime`**

Remove `RevisionTarget` from `crates/agent-runtime/src/types.rs`. Add GrokRxiv-owned revision target parsing under `agenthero/apps/grokrxiv/crates/orchestrator/src/revision_targets.rs` if that module still needs it.

- [ ] **Step 3: Remove `GROKRXIV_BIND` from platform config**

Change `crates/orchestrator/src/config.rs` to read only `AGENTHERO_BIND` for the generic platform server. Add a GrokRxiv-owned compatibility note to `agenthero/apps/grokrxiv/README.md` if the old env var is still useful for the app-owned runtime.

- [ ] **Step 4: Remove `review_id` from generic app-run tables**

In `crates/orchestrator/src/cli.rs`, replace the app-run table column `review_id` with `run_key`. Populate it from:

```rust
fn app_run_operator_key(input: &serde_json::Value) -> Option<String> {
    input.pointer("/values/operator_key")
        .or_else(|| input.pointer("/values/review_id"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}
```

Use label `run_key` in generic output. GrokRxiv can set `operator_key` to a review id inside its adapter input.

- [ ] **Step 5: Run app-neutrality and manifest tests**

Run:

```bash
cargo test -p agenthero-dag-runtime --test manifest -- --nocapture
cargo test -p agenthero-orchestrator --test agenthero_cli_contract -- --nocapture
cargo test -p agenthero-orchestrator --test runtime_hardening_contract -- --nocapture
```

Expected: the app-neutrality guard passes. Existing GrokRxiv app actions still parse through app manifests.

- [ ] **Step 6: Commit app-neutral cleanup**

Run:

```bash
git add crates/dag-runtime/src/lib.rs crates/agent-runtime/src/types.rs \
  crates/orchestrator/src/config.rs crates/orchestrator/src/cli.rs \
  crates/dag-runtime/tests/manifest.rs crates/orchestrator/tests/agenthero_cli_contract.rs \
  crates/orchestrator/tests/runtime_hardening_contract.rs agenthero/apps/grokrxiv/README.md
git commit -m "refactor: remove GrokRxiv vocabulary from AgentHero platform crates"
```

Expected: commit succeeds.

---

### Task 9: Shrink The GrokRxiv Legacy Runtime Bridge

**Files:**
- Modify: `agenthero/apps/grokrxiv/rust/src/main.rs`
- Modify: `agenthero/apps/grokrxiv/rust/tests/adapter.rs`
- Modify: `agenthero/apps/grokrxiv/dags/review-loop.yaml`
- Modify: `agenthero/apps/grokrxiv/app.yaml`
- Modify: `docs/dag-abstraction-gap-analysis.md`

**Interfaces:**
- Consumes: existing GrokRxiv review/formalize DAG manifests and app-owned crates.
- Produces: migrated arXiv review and formalize lanes that execute through `DagExecutor` without shelling to the legacy `grokrxiv-app` runtime.

- [ ] **Step 1: Add an adapter test that rejects the legacy route for arXiv review**

Add to `agenthero/apps/grokrxiv/rust/tests/adapter.rs`:

```rust
#[test]
fn arxiv_review_requests_execute_manifest_dag_without_legacy_runtime_route() {
    let request = request_for("review", "review-loop", vec![
        "2606.24837".to_string(),
        "--type".to_string(),
        "arxiv".to_string(),
        "--no-external-actions".to_string(),
    ]);

    assert!(
        manifest_dag_requested_for_test(&request),
        "arXiv review must execute the manifest DAG path"
    );
}
```

Expose a `#[cfg(test)]` helper from `agenthero/apps/grokrxiv/rust/src/main.rs` if needed:

```rust
#[cfg(test)]
pub fn manifest_dag_requested_for_test(request: &AppAdapterRequest) -> bool {
    manifest_dag_requested(request)
}
```

- [ ] **Step 2: Make arXiv review stay on the manifest DAG path**

In `agenthero/apps/grokrxiv/rust/src/main.rs`, update `manifest_dag_requested` so `review` requests with `--type arxiv` or inferred arXiv ids always run the manifest path, even when not dry-run. Keep non-arXiv source types behind the existing bridge until each lane has a manifest handler.

- [ ] **Step 3: Move forced `--loop` behavior into the manifest action**

Remove adapter-level forced `--loop` injection for the migrated arXiv lane. Represent loop behavior in `agenthero/apps/grokrxiv/dags/review-loop.yaml` and app action metadata instead.

- [ ] **Step 4: Add formalize manifest-path coverage**

Add a test that `formalize <review_id> --auto-detect --no-external-actions` executes the manifest DAG path and emits `app_action.started` and terminal lifecycle events with all mandatory trace fields.

- [ ] **Step 5: Update bridge documentation**

In `docs/dag-abstraction-gap-analysis.md`, replace the broad "live paper-review must migrate" note with a lane matrix:

```text
Migrated through manifest DAG:
- arXiv review
- formalize auto-detect

Still behind GrokRxiv legacy bridge:
- local PDF review
- local TeX review
- git/corpus review
- publish side effects not yet represented as DAG tools
```

- [ ] **Step 6: Run GrokRxiv adapter tests**

Run:

```bash
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml --test adapter -- --nocapture
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace -- --nocapture
```

Expected: GrokRxiv adapter tests pass. ArXiv review and formalize lanes no longer route to the legacy runtime helper.

- [ ] **Step 7: Commit GrokRxiv bridge shrink**

Run:

```bash
git add agenthero/apps/grokrxiv/rust/src/main.rs agenthero/apps/grokrxiv/rust/tests/adapter.rs \
  agenthero/apps/grokrxiv/dags/review-loop.yaml agenthero/apps/grokrxiv/app.yaml \
  docs/dag-abstraction-gap-analysis.md
git commit -m "refactor: move GrokRxiv arXiv review onto manifest runtime"
```

Expected: commit succeeds.

---

### Task 10: Refresh Observability Docs And Completion Audit

**Files:**
- Modify: `docs/agenthero-observability-operator-guide.md`
- Modify: `docs/agenthero-platform-completion-audit.md`

**Interfaces:**
- Consumes: final implemented runtime behavior and acceptance evidence.
- Produces: truthful operator docs that do not overclaim exactly-once or complete GrokRxiv migration.

- [ ] **Step 1: Update the operator guide**

Update `docs/agenthero-observability-operator-guide.md` with:

```markdown
## Crash Recovery And Resume

AgentHero recovers expired app-run leases by fencing stale workers, attaching a
reconstructed checkpoint when completed node attempts are durable, and requeueing
the same app run. Recovered attempts reuse the app-run idempotency key and must
not repeat completed nodes whose outputs are present in the checkpoint.
```

Add a command to inspect recovery:

```bash
agh --json app events <APP_RUN_ID> \
  | jq '.events[] | select(.event_type | startswith("app_run.resume") or contains("lease_expired"))'
```

- [ ] **Step 2: Update the completion audit status language**

Change `docs/agenthero-platform-completion-audit.md` so local durability is described as:

```text
Implemented for fenced local workers with checkpoint resume when completed node
attempts have durable output; not a distributed exactly-once guarantee.
```

List any GrokRxiv source lanes still on the legacy bridge in a "Remaining runtime migration" section.

- [ ] **Step 3: Add loom invariant notes**

Add a short section:

```markdown
## Scheduler Race Tests To Add With Loom

- A stale worker cannot complete after its lease was expired and replaced.
- Cancellation observed while adapter stderr is draining does not lose terminal events.
- Lease renewal failure followed by recovery does not produce two active leases for one app run.
```

- [ ] **Step 4: Run docs lint checks**

Run:

```bash
git diff --check
```

Expected: no whitespace errors.

- [ ] **Step 5: Commit docs**

Run:

```bash
git add docs/agenthero-observability-operator-guide.md docs/agenthero-platform-completion-audit.md
git commit -m "docs: document AgentHero fenced resume semantics"
```

Expected: commit succeeds.

---

### Task 11: Run The Full Verification Matrix

**Files:**
- No source edits unless verification exposes failures.

**Interfaces:**
- Consumes: all code, migrations, app manifests, docs.
- Produces: current evidence that the runtime hardening milestone passes.

- [ ] **Step 1: Run formatting and core Rust checks**

Run:

```bash
cargo fmt --check
cargo check --workspace
```

Expected: both commands exit `0`.

- [ ] **Step 2: Run generic platform tests**

Run:

```bash
cargo test -p agenthero-dag-runtime --test manifest -- --nocapture
cargo test -p agenthero-dag-executor -- --nocapture
cargo test -p agenthero-orchestrator --test app_runtime_schema -- --nocapture
cargo test -p agenthero-orchestrator --test dag_app_registry -- --nocapture
cargo test -p agenthero-orchestrator --test agenthero_cli_contract -- --nocapture
cargo test -p agenthero-orchestrator --test runtime_hardening_contract -- --nocapture
cargo test -p agenthero-orchestrator --lib -- --nocapture
```

Expected: all tests exit `0`.

- [ ] **Step 3: Run app workspace checks**

Run:

```bash
cargo check --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace
cargo check --manifest-path agenthero/apps/c2rust/Cargo.toml --workspace
cargo check --manifest-path agenthero/apps/platform-smoke/Cargo.toml --workspace
cargo test --manifest-path agenthero/apps/grokrxiv/Cargo.toml --workspace -- --nocapture
cargo test --manifest-path agenthero/apps/c2rust/Cargo.toml --workspace -- --nocapture
cargo test --manifest-path agenthero/apps/platform-smoke/Cargo.toml --workspace -- --nocapture
```

Expected: all commands exit `0`.

- [ ] **Step 4: Run app catalog and action smokes**

Run:

```bash
cargo run -p agenthero-orchestrator --bin agh -- --json app run grokrxiv
cargo run -p agenthero-orchestrator --bin agh -- --json app run c2rust
cargo run -p agenthero-orchestrator --bin agh -- --json app run platform-smoke
```

Expected: each command returns an action catalog in JSON and does not invoke an adapter action.

- [ ] **Step 5: Run cancellation and observability smokes with database configured**

Run with the repo `.env` loaded:

```bash
set -a
. ./.env
set +a
cargo run -p agenthero-orchestrator --bin agh -- --json app run platform-smoke cancellation-smoke
```

Capture the run id, cancel it:

```bash
cargo run -p agenthero-orchestrator --bin agh -- --json app cancel <APP_RUN_ID> --reason runtime-hardening-smoke
cargo run -p agenthero-orchestrator --bin agh -- --json app events <APP_RUN_ID> \
  | jq '[.events[] | select((["app_run_id","dag_run_id","node_id","attempt","node_kind","tool_id","manifest_hash","artifact_id","lease_id","status","exit_status","duration_ms"] - (.payload | keys)) | length > 0)] | length'
```

Expected: final command prints `0`.

- [ ] **Step 6: Run metrics smoke**

Start the server:

```bash
cargo run -p agenthero-orchestrator --bin agh -- serve
```

In another shell:

```bash
curl -fsSL http://127.0.0.1:8787/metrics \
  | rg "agenthero_app_runs|agenthero_dag_runs|agenthero_scheduler_lease_renewals_total|agenthero_dag_node_duration_seconds"
```

Expected: all metric names are present.

- [ ] **Step 7: Run final diff checks**

Run:

```bash
git diff --check
git status --short
```

Expected: no whitespace errors. `git status --short` only shows intentional final changes if a previous task found and fixed a verification failure.

- [ ] **Step 8: Commit final verification fixes**

If Step 7 shows intentional source fixes, run:

```bash
git add <changed-files>
git commit -m "fix: complete AgentHero runtime hardening verification"
```

Expected: final branch contains contract, implementation, docs, and verification commits.
