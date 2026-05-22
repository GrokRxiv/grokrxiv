# AgentHero

AgentHero is a Rust/Tokio DAGOps runtime for agentic applications as DAGs.
The root of this repository is the platform control plane: app discovery,
manifest validation, generic DAG execution, process-adapter dispatch, runtime
state, and worker-facing orchestration contracts.

GrokRxiv is now an installed DAGOps app, not the root product. Its app
manifests, web app, migrations, scripts, skills, Docker stack, prompts,
schemas, and domain crates live under `agenthero/apps/grokrxiv/`.

## Root Layout

```text
crates/                    AgentHero platform crates
agenthero/apps/            Installed DAGOps apps
agenthero/migrations/      Generic platform runtime migrations
supabase/migrations/       Combined migration view for local Supabase
.agenthero/                Ignored local runtime artifacts
```

## CLI

```sh
cargo run -p agenthero-orchestrator -- app list
cargo run -p agenthero-orchestrator -- app run grokrxiv
cargo run -p agenthero-orchestrator -- app run grokrxiv review 2605.17307
cargo run -p agenthero-orchestrator -- app run c2rust migrate --dry-run
```

Installed app actions are declared in `agenthero/apps/<app>/app.yaml`. Running
`agh --json app run <app>` with no action prints that app's action catalog,
including any Vercel deployment metadata for generated websites.

## GrokRxiv

GrokRxiv app documentation starts at:

```text
agenthero/apps/grokrxiv/README.md
```

Local GrokRxiv env templates live in `agenthero/apps/grokrxiv/env/`. The root
`.env.example` is only the AgentHero selector that points at app-local env
files through `AGENTHERO_ENV_FILES`.

Rendered runtime artifacts are written under:

```text
.agenthero/artifacts/<app>/...
```

GrokRxiv extraction/source artifacts remain durable data-repo content under
`GROKRXIV_DATA_REPO_PATH`.
