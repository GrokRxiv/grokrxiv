# AgentHero Observability Operator Guide

AgentHero app runs must be auditable from AgentHero itself. Operators should not
need `ps`, `lsof`, `pgrep`, or ad hoc process inspection to know what happened.
The durable surfaces are:

- `agh app status <APP_RUN_ID>`
- `agh app events <APP_RUN_ID>`
- `agh app logs <APP_RUN_ID>`
- `/app-runs/<APP_RUN_ID>/events`
- `/app-runs/<APP_RUN_ID>/events/stream`
- `/app-runs/<APP_RUN_ID>/logs`
- `/metrics`

## Required Trace Fields

Every durable event payload and app-adapter report event must expose these
fields, even when the value is null:

| Field | Meaning |
| --- | --- |
| `app_run_id` | Product app action run id. |
| `dag_run_id` | DAG manifest execution id. |
| `node_id` | Node id, or null for DAG/app lifecycle events. |
| `attempt` | Node attempt number, or null for DAG/app lifecycle events. |
| `node_kind` | Generic node kind such as `tool`, `verify`, `loop`, or `dag_call`. |
| `tool_id` | Generic tool id when a node uses one. |
| `manifest_hash` | Hash of the DAG manifest used for replay and audit. |
| `artifact_id` | Current artifact id when an event is artifact-specific. |
| `lease_id` | Worker lease that owned the app/DAG run. |
| `status` | Node, DAG, or app action status. |
| `exit_status` | Process/verifier exit status when applicable. |
| `duration_ms` | Node duration when known. |

AgentHero may include additional fields such as `input_refs`, `output_refs`,
`diagnostic_refs`, `command`, `model`, `prompt_hash`, `executor`, `role`,
`policy`, and `trace`. Those are additive audit fields. Apps must not depend on
missing required fields to mean anything; missing required fields are a runtime
bug.

## Operator Checks

List recent runs:

```bash
agh --json app runs --app grokrxiv --action review --limit 5
```

Inspect one run:

```bash
agh --json app status <APP_RUN_ID>
```

Check the trace-field contract:

```bash
agh --json app events <APP_RUN_ID> \
  | jq '{
      run_id,
      event_count: (.events | length),
      missing_trace_field_events: [
        .events[]
        | select(([
            "app_run_id",
            "dag_run_id",
            "node_id",
            "attempt",
            "node_kind",
            "tool_id",
            "manifest_hash",
            "artifact_id",
            "lease_id",
            "status",
            "exit_status",
            "duration_ms"
          ] - (.payload | keys)) | length > 0)
        | {id, event_type, node_id}
      ]
    }'
```

`missing_trace_field_events` must be empty.

Inspect logs:

```bash
agh app logs <APP_RUN_ID> --tail 80
```

The durable text log includes human-readable lifecycle lines plus structured
JSONL records prefixed with:

```text
@@AGENTHERO_EVENT
```

Follow events during a run:

```bash
agh app events <APP_RUN_ID> --follow
```

Follow logs during a run:

```bash
agh app logs <APP_RUN_ID> --follow
```

## Determinism Checks

`agh app status <APP_RUN_ID>` must expose a determinism block for persisted DAG
runs:

- `manifest_hash`
- `frozen_input_hash`
- `dag_output_hash`
- `node_input_hashes`
- `node_output_hashes`
- `artifacts_with_sha256`
- `artifacts_missing_sha256`
- `checkpoint_available`
- `replay_ready`
- `compare_ready`

For a completed run, `artifacts_missing_sha256` should be `0` unless an app has
explicitly documented why a volatile artifact cannot be hashed. `replay_ready`
and `compare_ready` must not be inferred from the app's domain result; they are
AgentHero runtime facts.

## App Adapter Contract

Each app manifest must declare the observability surface:

```yaml
observability:
  events: true
  logs: true
  status: true
  event_stream: true
  lifecycle_events:
    - app_action.started
    - app_action.completed
    - app_action.failed
  trace_fields:
    - app_run_id
    - dag_run_id
    - node_id
    - attempt
    - node_kind
    - tool_id
    - manifest_hash
    - artifact_id
    - lease_id
    - status
    - exit_status
    - duration_ms
```

Process adapters must:

- read one `agenthero.app.v1` request from stdin
- write one `agenthero.app.v1` response to stdout
- write structured runtime events to stderr with the `@@AGENTHERO_EVENT `
  prefix
- emit `app_action.started`
- emit `app_action.completed` or `app_action.failed`
- include all required trace fields on lifecycle events and node events
- return a `DagExecutionReport` whose `events[]` also includes all required
  trace fields

Generic node events should record:

- `input_refs`
- `output_refs`
- `diagnostic_refs`
- `command`
- `exit_status`
- `model`
- `prompt_hash`
- `manifest_hash`
- `lease_id`

When a value does not apply, use null or an empty object. Do not omit the
required trace key.

## Verification Routing

AgentHero records verifier execution. Apps interpret verifier meaning.

Examples:

- A Lean node records `command`, `exit_status`, stdout/stderr, Lean source
  artifacts, and verifier reports.
- Formal-proofs decides whether that evidence means a certificate is trusted.
- GrokRxiv decides whether a Lean run affects a paper-review artifact.
- C2Rust decides whether compile/lint/fuzz evidence is enough for migration
  confidence.

AgentHero must not encode theorem meaning, C-to-Rust safety meaning, or paper
review meaning. It only records that the verifier ran, what it consumed, what it
produced, and how it exited.

## Smoke Matrix

Use these as low-cost platform checks after changing the runtime, scheduler,
adapter protocol, logging, or app manifests.

GrokRxiv no-lean:

```bash
agh --json app run grokrxiv review 2606.24837 \
  --type arxiv \
  --no-lean \
  --no-external-actions
```

Expected evidence:

- app run state is `done`
- `lean_policy.disabled == true`
- Lean targets may exist
- Lean result artifact has `skipped == true`
- compile command and exit status are null
- events have no missing trace fields

GrokRxiv auto-detect Lean:

```bash
agh --json app run grokrxiv review 2606.24837 \
  --type arxiv \
  --no-external-actions
```

Expected evidence:

- app run state is `done`
- `lean_policy.auto_detect == true`
- if Lean targets exist, the Lean event records a command such as
  `["lean", "review_loop/lean/GrokRxiv/Proofs.lean"]`
- `exit_status` is recorded
- proof status is interpreted by GrokRxiv artifacts, not by AgentHero

C2Rust:

```bash
agh --json app run c2rust migrate agenthero/apps/c2rust/rust/src/lib.rs
```

Expected evidence:

- app run state is `done`
- events have no missing trace fields
- report events have no missing trace fields
- artifacts are hashed
- `replay_ready == true`

Formal-proofs:

```bash
agh --json app run formal-proofs theorem-triage --target e677-fin-e255
```

Expected evidence:

- app run state is `done`
- events have no missing trace fields
- target, status, and report artifacts are persisted and hashed
- `replay_ready == true`

## No Process Inspection

Runtime cleanup, monitoring, and app audit must use AgentHero state:

- app run state
- DAG run state
- node states
- worker leases
- events
- logs
- artifact rows
- deterministic hashes

Do not implement operator status or cleanup checks by scraping `ps`, `lsof`, or
`pgrep`. If the required information is not visible through AgentHero, add it to
the AgentHero runtime state, event stream, logs, or metrics surface.
