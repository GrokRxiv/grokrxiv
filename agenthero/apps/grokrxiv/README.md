# GrokRxiv DAGOps App

GrokRxiv is the research ingest, extraction, review, revise, validate, and
publish DAG app that proves the AgentHero runtime at scale.

## App Layout

```text
app.yaml        app actions and Vercel deployment metadata
dags/           paper-ingest, paper-extract, paper-review, revise, publish
agents/         YAML agent role configs
prompts/        prompt templates
schemas/        strict JSON contracts
rust/           AgentHero process adapter
crates/         GrokRxiv domain/runtime crates
web/            Vercel Next.js app
infra/          app-local Docker, LiteLLM, Supabase, Railway config
env/            app-local env templates
migrations/     GrokRxiv projection/business SQL
scripts/        app-local smoke and operator helpers
skills/         strict JSON reviewer skill installer
tests/          GrokRxiv app E2E fixtures and smoke tests
```

## CLI

```sh
agh app run grokrxiv
agh app run grokrxiv extract 2605.17307
agh app run grokrxiv review 2605.17307 --type arxiv
agh app run grokrxiv approve <REVIEW_ID>
agh app run grokrxiv validate citations
```

Runtime render artifacts are written to
`.agenthero/artifacts/grokrxiv/reviews/<review_id>/`. Source and extraction
artifacts remain in `GROKRXIV_DATA_REPO_PATH`.

## Env

From the repo root:

```sh
cp .env.example .env
cd agenthero/apps/grokrxiv/env
for f in .env_*.example; do cp "$f" "${f%.example}"; done
```

The root `.env` lists the app env files through `AGENTHERO_ENV_FILES`.

## Local Stack

```sh
supabase start
bash agenthero/apps/grokrxiv/infra/supabase/setup.sh
docker compose -f agenthero/apps/grokrxiv/infra/docker-compose.yml up --build
```

## Vercel

The GrokRxiv website is declared in `app.yaml` as a Vercel deployment:

```text
root: web
project: grokrxiv
framework: nextjs
build_command: pnpm --filter @grokrxiv/web build
```

In Vercel, set the project root directory to
`agenthero/apps/grokrxiv/web`. AgentHero app metadata exists so future
generated-site apps can expose the same deploy contract without root-level
special cases.
