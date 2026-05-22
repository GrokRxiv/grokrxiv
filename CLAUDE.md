# AgentHero / GrokRxiv — Claude Conventions

## Product Boundary

AgentHero is the root platform: Rust/Tokio control plane, DAGOps app discovery,
generic executor contracts, runtime state, worker scheduling, and process
adapter dispatch. GrokRxiv is the first installed DAGOps app and lives under
`agenthero/apps/grokrxiv/`.

Do not add GrokRxiv-specific lifecycle code, migrations, scripts, web files, or
domain crates back to the repository root. Root `crates/` must stay platform
only.

## App Layout

- `agenthero/apps/<app>/app.yaml` declares app actions and deployable surfaces.
- `agenthero/apps/<app>/dags/` contains DAG manifests.
- `agenthero/apps/<app>/agents/`, `prompts/`, and `schemas/` contain app-owned
  LLM contracts.
- `agenthero/apps/<app>/rust/` contains the process adapter.
- `agenthero/apps/grokrxiv/crates/` contains GrokRxiv domain/runtime crates.
- `agenthero/apps/grokrxiv/web/` is the GrokRxiv Vercel Next.js app.
- `agenthero/apps/grokrxiv/migrations/` contains GrokRxiv projection/business
  migrations.
- `agenthero/migrations/` contains generic AgentHero runtime migrations.

## Commands

Use app-scoped commands:

```sh
agh app run grokrxiv review <source>
agh app run grokrxiv extract <source>
agh app run grokrxiv approve <review_id>
agh app run c2rust migrate --dry-run
```

Do not add root commands such as `agh review` or resurrect the legacy
`grokrxiv` root CLI.

## Database

Every DAG app shares the AgentHero runtime tables: `app_runs`, `dag_runs`,
`dag_run_nodes`, `dag_artifacts`, `dag_events`, worker tables, and generic
cache tables. Product apps may own projection tables. GrokRxiv owns
`grokrxiv_*` projections plus legacy migration-era tables until fully migrated.

Apply migrations in order: platform migrations from `agenthero/migrations/`,
then app migrations from `agenthero/apps/grokrxiv/migrations/`. The
`supabase/migrations/` directory is a combined local Supabase view.

## Web Deployments

Website-generating apps declare deployment metadata in `app.yaml`.
For Vercel, the app owns:

- Vercel project name
- app-relative root directory
- framework/build/output metadata
- required environment variable names

AgentHero should schedule/generate the site, then hand Vercel the declared app
root. GrokRxiv's Vercel root is `agenthero/apps/grokrxiv/web`.

## Plan Run Workflow

For a new implementation plan, first checkpoint existing feature-branch work,
merge it locally to `main`, revalidate `main`, and create a fresh branch for
the new plan.

## Hard Rules

- Root orchestration code must not depend on concrete app crates.
- Agent identity is YAML/string DAG contract; do not add app-specific Rust role
  enums.
- Schemas, prompts, manifests, and catalogs are LLM-facing structural
  contracts and must stay explicit.
- App-specific tools belong in the app adapter or app-owned crates.
- Use `agh app run <app> <action>` for app-path smoke tests.
