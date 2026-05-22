# AgentHero / GrokRxiv Agent Instructions

## Plan Run Workflow

For a new implementation plan, first checkpoint any existing dirty work:

1. Commit the current feature branch.
2. Merge it locally to `main`.
3. Revalidate the merged `main` state.
4. Create a fresh branch for the new plan.

Do this before adding new plan changes whenever prior uncommitted or unfinished
work exists.

## Orchestration Model

AgentHero is the Rust/Tokio DAGOps control plane. Tokio is the async substrate
for local tasks, timers, networking, cancellation, timeouts, and worker I/O; it
is not the orchestration abstraction. The durable product abstractions are
`DagApp`, node, edge, artifact, capability, execution context, verifier, and
policy.

Agent chat sessions, CLI tools, Rust-native functions, non-Rust programs, cloud
workers, and future remote service nodes are workers behind Rust-controlled DAG
nodes. The GrokRxiv review/revise pipeline is the first DAG app proving the
abstraction, not the orchestration contract itself.

- DAG manifests live in `dags/*.yaml`.
- Generic execution contracts live in `crates/dag-executor`.
- Concrete DAG apps live in `crates/dag-app-*`.
- Agent configs live in `agents/<dag-type>/*.yaml`.
- Prompt templates live in `prompts/`.
- Output contracts live in `schemas/*.schema.json` and must be
  LLM-readable, strict, and contract-preserving.
- Rust-native DAG tool handlers are registered in
  `crates/orchestrator/src/dag_tools.rs`.
- Extraction-agent callable tools live under
  `crates/orchestrator/src/agents/extraction/tools/` or the owning extraction
  agent module.
- Role identity is a DAG/YAML string contract. Do not introduce Rust enums for
  app-specific agent roles.
- Node I/O at the executor boundary is named JSON values plus artifact
  references. App crates may convert those values into typed Rust structs, but
  generic executor code must not depend on paper/review/arXiv types.

Manifest rules:

- `tools:` declares executable tools. Use `executor: rust` with `handler:` for
  Rust-native functions, or `executor: cli` with `command:` for subprocess
  tools.
- `roles:` declares agent identities and points at YAML configs.
- `nodes:` declares DAG nodes. Tool nodes must reference a declared tool.
  Agent/verify/synthesizer nodes must reference a declared role.
- `edges:` is the execution topology. Add/remove tools and agents by editing
  manifests, not by hardcoding supervisor control flow.
- `dag_call` composes DAGs. Prefer a separate DAG plus `dag_call` when a
  pipeline can stand alone, such as citation validation.
- A new DAG app needs a manifest plus an app crate. Register the app through
  the orchestrator DAG app registry; do not add a one-off supervisor branch.
- The scheduler/executor may place work on local Tokio tasks, local CLI
  subprocesses, Rust handlers, cloud runners, local inference, or future remote
  AgentHero worker nodes. DAG apps must not depend on a paper-review-specific
  supervisor branch.
- Distributed runtime work belongs in the AgentHero control plane: worker
  registry, node assignment, artifact/state store, retries/checkpoints,
  capability permissions, logs/tracing, and remote execution protocol.

## Product App CLI

The operator CLI is app-scoped:

```bash
agh app list
agh app show grokrxiv
agh app run grokrxiv -- extract 2605.17307
agh app run grokrxiv -- review 2605.17307 --type arxiv
agh app run grokrxiv -- approve <REVIEW_ID>
agh app run c2rust -- migrate --input src/main.c
```

Do not add new unscoped root commands such as `agh review` or `agh approve`.
Add an app action in `crates/orchestrator/src/dag_apps.rs`, route it through
the generic app runner/adapter, and keep the action mapped to a DAG type. Root
commands are reserved for platform operations such as `app`, `serve`, `doctor`,
`config`, `dag`, `agent`, and `jobs`.

## Runtime Database Shape

Every DAG does **not** get its own scheduler table set. Runtime state is shared:

- `app_runs` tracks product app actions.
- `dag_runs` tracks manifest executions under an app run.
- `dag_run_nodes` tracks node attempts and statuses.
- `dag_artifacts` stores named artifact references.
- `dag_events` stores runtime events.
- `worker_nodes` and `worker_leases` support distributed runners.
- `agent_output_cache` is keyed by app, DAG type, node, role, runner, model,
  and input hash.

DAG apps may have projection/business tables when the product needs queryable
domain state. The GrokRxiv app uses `grokrxiv_sources`, `grokrxiv_reviews`, and
`grokrxiv_moderation_queue` projections, but those tables are not the generic
executor contract.

## LLM-Readable Contracts

This is an LLM-built product. Manifests, schemas, prompts, agent configs, and
Rust handler catalogs are structural contracts for both LLM and human
contributors. They must be explicit enough that an LLM can add or modify a
tool, agent, or DAG without guessing hidden shape.

Rules:

- Use boring, literal names that line up across DAG node ids, tool ids, handler
  ids, artifact filenames, schema fields, and test names.
- Keep contract files self-describing; do not rely on chat context, stale plan
  notes, or unstated conventions.
- Do not add undeclared JSON fields, optional-by-omission fields, or schema drift
  to make a single model response pass.
- When a shape changes, update the manifest, schema, prompt, Rust type/catalog,
  and tests together.
- Prefer small focused files and directories over dumping more orchestration
  logic into `cli.rs` or one monolithic agents file.

## Strict JSON Agent Output

When an AgentHero prompt includes an output schema, the schema is the contract:

- Required properties are required.
- Enum values use the exact listed strings and casing.
- Arrays of objects contain objects, not free-form strings.
- Numeric fields are numbers, not strings.
- Closed schemas do not allow undeclared fields.
- Nullable fields must still appear and use `null` when no value applies.

Review roles have these top-level output shapes:

| Role | Top-level required fields |
|------|---------------------------|
| `summary` | `tldr`, `plain_language_summary`, `key_contributions[]`, `audience` |
| `technical_correctness` | `claims[]`, `overall_correctness`, `confidence` |
| `novelty` | `novelty_score`, `related_work[]`, `missing_prior_art[]`, `verdict`, `confidence` |
| `reproducibility` | `code_availability`, `code_url`, `data_availability`, `data_url`, `environment`, `concerns[]`, `reproducibility_score`, `confidence` |
| `citation` | `entries[]`, `missing_references[]`, `summary`, `confidence` |
| `meta_reviewer` | `summary`, `strengths[]`, `weaknesses[]`, `questions[]`, `recommendation`, `confidence` |

For `novelty`, each `related_work[]` item is exactly
`{citation_key, title, relation, delta}`.

For `citation`, each `entries[]` item is exactly
`{citation, exists, resolved_doi, resolved_url, relevance, notes, explanation}`.

The orchestrator validates outputs and may issue one corrective retry. Agents
must emit raw JSON; the first character of stdout is `{`.

## Adding A Rust Tool

1. Add or scaffold the manifest tool:
   `agh dag add-tool --dag-type <dag> --tool-id <id> --executor rust --handler <module>::<function> --after <node> --before <node> --input <artifact> --output <artifact> --write`
2. Register the handler in `crates/orchestrator/src/dag_tools.rs`.
3. Implement the function in the owning Rust module.
4. Add tests for the function and manifest validation.
5. Run `agh validate --dag-type <dag>`.

## Adding A DAG App

1. Add `dags/<dag-type>.yaml`.
2. Add `crates/dag-app-<dag-type>/` implementing `agenthero_dag_executor::DagApp`.
3. Add the crate to the workspace.
4. Register the app in `crates/orchestrator/src/dag_apps.rs`.
5. Register the product app/action surface in `crates/orchestrator/src/dag_apps.rs`
   if it should be callable through `agh app run <app> -- <action>`.
6. Add a smoke test that runs the manifest through
   `agenthero_dag_executor::DagExecutor`.
7. Run `agh dag run --dag-type <dag-type> --json`.

## Adding A CLI Tool

1. Add the manifest tool with `executor: cli` and `command: [...]`.
2. Declare stable `inputs:` and `outputs:` on the node.
3. Keep subprocess input/output JSON schema-compatible.
4. Add a dry-run/fixture test; do not require live network in unit tests.

## Adding An Agent

1. Add an agent YAML under `agents/<dag-type>/<role-id>.yaml`.
2. Add prompt and schema files.
3. Declare `prompt_context`, `system_overlays`, `verifiers`, and
   `postprocessors` explicitly when the agent needs reusable Rust hook
   behavior.
4. Add the role and node to the DAG manifest.
5. Use `<dag-type>.<role-id>` as the durable role key.
6. Validate output against the schema; emit raw JSON when invoked with an
   output schema.

## Verification

Minimum checks for DAG/tool work:

```bash
cargo test -p agenthero-dag-runtime --test manifest
cargo test -p agenthero-dag-executor
cargo test -p agenthero-orchestrator --test dag_app_registry
cargo test -p agenthero-orchestrator --lib --features full -- --test-threads=1
cargo check -p grokrxiv-ingest -p agenthero-dag-runtime -p grokrxiv-storage -p agenthero-orchestrator --features full
cargo run -p agenthero-orchestrator --features full --bin agh -- validate --dag-type <dag>
cargo run -p agenthero-orchestrator --features full --bin agh -- --json dag run --dag-type <dag>
```

Update or remove tests that encode obsolete fixed-pipeline assumptions. Keep
tests that protect public behavior, schema contracts, and DAG validation.
