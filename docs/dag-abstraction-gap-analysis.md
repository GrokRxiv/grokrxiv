# DAG-APP Executor Architecture Status

This document replaces the older gap analysis. The target is a distributed
DAG-app runner: Rust schedules DAG manifests, and concrete apps provide tools,
agents, verifiers, renderers, publishers, and artifact adapters.

## Current Boundary

- `crates/dag-runtime` parses and validates YAML manifests.
- `crates/dag-executor` executes validated manifests over generic named JSON
  values plus artifact references.
- `crates/dag-app-*` crates own concrete DAG app adapters.
- `crates/orchestrator` owns CLI, HTTP, DB, scheduler, jobs, and the DAG app
  registry.
- `ingest`, `render`, `publisher`, `schemas`, `verifier`, and `storage` are
  domain/tool/provider crates behind app adapters, not executor dependencies.

Registered DAG apps:

- `paper-ingest`
- `paper-extract`
- `paper-review`
- `paper-revise`
- `paper-publish`
- `citation-validation`
- `c-to-rust`

`paper-extract` now starts with a `dag_call` to `paper-ingest`. `c-to-rust`
runs through the same generic executor path and is the non-paper proof that the
executor is not paper-review-shaped.

## Migration Rule

The research flow is the proving-ground DAG app chain:

`paper-ingest -> paper-extract -> paper-review -> paper-revise -> paper-publish`

The live review pipeline must migrate to the executor path. Review-specific
code may remain only as app adapter behavior while it is being moved behind
node handlers. It must not be treated as the permanent executor shape.

## Adding Capabilities

- Add topology in `dags/*.yaml`.
- Add agents in `agents/<dag-type>/*.yaml`.
- Add Rust functions as manifest tools plus handler catalog entries.
- Add CLI/non-Rust tools with explicit command, inputs, and outputs.
- Add new apps as `crates/dag-app-*` plus an orchestrator registry entry.
- Keep schemas, prompts, manifests, and tests synchronized because they are
  LLM-facing structural contracts.

## Remaining Work

- Move live `paper-review` node behavior out of `review_flow` and behind
  executor node handlers.
- Move extraction stage ordering fully out of `ingest_pipeline.rs` and into
  `paper-extract.yaml` execution.
- Dispatch real Rust tool handlers from the executor path instead of the
  current manifest-only app smoke adapters.
- Add remote worker leases, heartbeats, capabilities, retry ownership, and
  placement for distributed service nodes.
- Add scaffold commands for DAG apps, agents, schemas, prompts, and Rust tools.
