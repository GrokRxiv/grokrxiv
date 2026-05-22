# DAG Abstraction Gap Analysis

Status: updated after the string/YAML agent migration on
`feature/orchestrator-deep-refactor`.

## Target Architecture

GrokRxiv is a distributed DAG-app runner. Rust owns orchestration, scheduling,
Tokio concurrency, persistence, verifier gates, and tool dispatch. DAG apps
declare their own shape in YAML:

- `dags/*.yaml` declares DAG types, tools, roles, nodes, gates, and edges.
- `agents/<dag-type>/*.yaml` declares agent ids, providers, runners, prompt
  templates, schemas, verifier ladders, context blocks, overlays, and
  postprocessors.
- `prompts/*.md` and `schemas/*.schema.json` are LLM-facing contracts.
- Rust-native functions are registered tool handlers; CLI and non-Rust tools
  are subprocess/external workers.

The research `ingest -> extract -> review -> output` flow is the first DAG app
proving the abstraction. Other DAG apps, including `c-to-rust`, should be able
to schedule onto the same Rust orchestration substrate without adding fixed
pipeline branches.

## Closed

| Area | Result |
|---|---|
| Agent identity | Review/runtime maps are keyed by DAG role id strings. The shared schema enum was removed. |
| Agent config authority | Runtime registry, schemas, verifier ladders, prompt paths, context blocks, overlays, and postprocessors load from YAML. |
| Review topology | `paper-review.yaml` supplies specialist roles through `feeds_meta=true`, the synthesizer role, and gate quorum. |
| Review behavior migration | Existing summary/technical/novelty/repro/citation/meta behavior is preserved through YAML + prompt templates + named context/postprocessor hooks. |
| Failure routing | Fatal review DAG failures transition reviews to `system_failed`. |
| Legacy DAG topology | The old Rust-only review DAG module was deleted. |
| Tests | Outdated fixed-role prompt/API integration tests were removed or migrated to string/YAML contracts. |

## Remaining Gaps

1. **Generic executor coverage is incomplete.**
   The review path now consumes manifest/YAML role metadata, but execution is
   still a review-specific function. The generic DAG executor still needs
   branches for `prepare_inputs`, `ingest_source`, `artifact`, `tool`,
   `dag_call`, `agent`, `synthesizer`, `verify`, `gate`, `render_artifacts`,
   and `moderation_ready`, with a `DagRunReport` emitted for every run.

2. **Extraction still mixes manifest authority with imperative stage code.**
   `paper-extract.yaml` declares the intended extraction DAG, but
   `ingest_pipeline.rs` still owns stage ordering. Move selection/order to the
   manifest and keep Rust stages as registered handlers.

3. **Tool handler registry needs to be the executor boundary.**
   `dag_tools.rs` catalogs handlers, but runtime dispatch still needs a stable
   `handler -> function` call surface with typed JSON input/output and dry-run
   tests for adding/removing tools.

4. **Distributed worker nodes are not first-class yet.**
   Runner backends exist (`api`, `cli`, `cloud`, `local_inference`), but the
   scheduler does not yet model remote service nodes, leases, heartbeats,
   worker capabilities, retry ownership, or per-node DAG app placement.

5. **Prompt/context hook names need docs and scaffolding.**
   The current named hooks are explicit and validated, but `grokrxiv dag
   scaffold-agent` should generate the YAML, prompt, schema, and test skeletons
   so LLMs and humans do not invent hidden contract shape.

6. **Review/revise is still the only scaled DAG app.**
   Add `c-to-rust` as the next DAG app to prove non-paper workloads can run on
   the same orchestrator with Rust-native tools, CLI tools, agents, gates, and
   outputs.

## Current Review DAG Contract

`dags/paper-review.yaml` is authoritative for:

- which roles feed synthesis (`feeds_meta: true`);
- which role synthesizes outputs (`kind: synthesizer`);
- quorum (`gate.min_usable`);
- node ids, gate ids, and DAG ordering.

`agents/paper-review/*.yaml` is authoritative for:

- runner/provider/model;
- prompt template path;
- input/output schema paths;
- verifier names;
- prompt context knobs such as body budget, bibliography mode, citation-context
  budget, and fact blocks;
- system overlays such as proof-as-code gates;
- output postprocessors such as citation verifier merge or novelty-fact merge.

Rust should add reusable hook functions, not role-specific conditionals. If a
new DAG app needs different behavior, add a named hook/tool and declare it in
YAML.

## Remediation Order

1. Implement the generic DAG executor and make paper-review call it.
2. Move paper-extract stage order and report emission fully behind that
   executor.
3. Add distributed worker-node scheduling: capabilities, leases, heartbeats,
   retries, and placement.
4. Add `grokrxiv dag scaffold-agent` and scaffold/tool validation docs.
5. Add `c-to-rust` as the second DAG app and test Rust-native + CLI + agent
   nodes end to end.
6. Remove remaining docs/comments that imply paper review is the orchestrator
   contract instead of one DAG app.
