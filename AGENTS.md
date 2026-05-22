# GrokRxiv Agent Instructions

## Plan Run Workflow

For a new implementation plan, first checkpoint any existing dirty work:

1. Commit the current feature branch.
2. Merge it locally to `main`.
3. Revalidate the merged `main` state.
4. Create a fresh branch for the new plan.

Do this before adding new plan changes whenever prior uncommitted or unfinished
work exists.

## Orchestration Model

Rust owns orchestration. Agent chat sessions, CLI tools, and other languages are
workers behind Rust-controlled DAG nodes.

- DAG manifests live in `dags/*.yaml`.
- Agent configs live in `agents/<dag-type>/*.yaml`.
- Prompt templates live in `prompts/`.
- Output contracts live in `schemas/*.schema.json` and must be
  LLM-readable, strict, and contract-preserving.
- Rust-native DAG tool handlers are registered in
  `crates/orchestrator/src/dag_tools.rs`.
- Extraction-agent callable tools live under
  `crates/orchestrator/src/agents/extraction/tools/` or the owning extraction
  agent module.

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

## Adding A Rust Tool

1. Add or scaffold the manifest tool:
   `grokrxiv dag add-tool --dag-type <dag> --tool-id <id> --executor rust --handler <module>::<function> --after <node> --before <node> --input <artifact> --output <artifact> --write`
2. Register the handler in `crates/orchestrator/src/dag_tools.rs`.
3. Implement the function in the owning Rust module.
4. Add tests for the function and manifest validation.
5. Run `grokrxiv dag validate --dag-type <dag>`.

## Adding A CLI Tool

1. Add the manifest tool with `executor: cli` and `command: [...]`.
2. Declare stable `inputs:` and `outputs:` on the node.
3. Keep subprocess input/output JSON schema-compatible.
4. Add a dry-run/fixture test; do not require live network in unit tests.

## Adding An Agent

1. Add an agent YAML under `agents/<dag-type>/<role-id>.yaml`.
2. Add prompt and schema files.
3. Add the role and node to the DAG manifest.
4. Use `<dag-type>.<role-id>` as the durable role key.
5. Validate output against the schema; emit raw JSON when invoked with an
   output schema.

## Verification

Minimum checks for DAG/tool work:

```bash
cargo test -p grokrxiv-dag-runtime --test manifest
cargo test -p grokrxiv-orchestrator --lib --features full -- --test-threads=1
cargo check -p grokrxiv-ingest -p grokrxiv-dag-runtime -p grokrxiv-storage -p grokrxiv-orchestrator --features full
cargo run -p grokrxiv-orchestrator --features full --bin grokrxiv -- dag validate --dag-type <dag>
```

Update or remove tests that encode obsolete fixed-pipeline assumptions. Keep
tests that protect public behavior, schema contracts, and DAG validation.
