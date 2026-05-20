# GrokRxiv

GrokRxiv is an agentic review pipeline for research papers. It ingests a paper,
builds reviewer context, runs schema-validated specialist reviewers, verifies
their outputs, renders review artifacts, and opens the GitHub PR used for human
moderation.

## What Works

- Review arXiv IDs and URLs with `grokrxiv review`.
- Review local `.tex` / `.pdf` files and git repositories with an explicit
  manuscript path.
- Run six current reviewer roles: `summary`, `technical_correctness`,
  `novelty`, `reproducibility`, `citation`, and `meta_reviewer`.
- Validate every role against a closed JSON schema and persist verifier status.
- Render HTML, Markdown, TeX, PDF, and zip artifacts under `artifacts/<review_id>/`.
- Open publication or revision-needed PRs based on the automated gate.
- Close, reject, request changes, approve, and publish reviews from the CLI.
- Create scheduled arXiv review batches and track every item through review and
  PR handoff.

The current runtime keeps `AgentRole`, the role schemas, verifier ladders,
cache keys, render paths, and database shape explicit. YAML-based runtime DAGs
and fully dynamic agent roles are planned, but they are not the production
orchestration path yet.

## Requirements

- Rust 1.82 or newer
- `pnpm`
- Docker
- Supabase CLI
- Pandoc for local TeX extraction
- Claude, Codex, Gemini, OpenAI, Anthropic, or local provider credentials for
  whichever runner/model profile you select

Local CLI runs load `.env` through `dotenvy`; keep `.env.example` as the public
template and put real secrets only in `.env`.

## Setup

```sh
pnpm install
cp .env.example .env
supabase start
cargo build --workspace
```

Run the local services:

```sh
cargo run -p grokrxiv-orchestrator --bin grokrxiv -- serve
cd apps/web && pnpm dev
```

The web app defaults to <http://localhost:3000>. The orchestrator defaults to
<http://localhost:8080>.

## Core CLI

Direct review:

```sh
grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307
```

Review a local manuscript:

```sh
grokrxiv review --runner cli --extractor cli --type tex ./paper.tex
grokrxiv review --runner cli --extractor cli --type pdf ./paper.pdf
```

Review a paper inside a git repository:

```sh
grokrxiv review --runner cli --extractor cli --type git \
  https://github.com/example/research-repo \
  --rev main \
  --paper-path papers/main.tex
```

Inspect and operate reviews:

```sh
grokrxiv list reviews --review-status awaiting_moderation --json
grokrxiv show <REVIEW_ID> --json
grokrxiv open <REVIEW_ID>
grokrxiv request-revisions <REVIEW_ID> --notes "Needs statistical correction."
grokrxiv approve <REVIEW_ID>
grokrxiv close <REVIEW_ID> --reason "Superseded by corrected review."
```

Job inspection:

```sh
grokrxiv jobs list --kind review --state running --json
```

## Batch Reviews

Batch reviews are for scheduled field sweeps such as 30 mathematics papers per
day from an arXiv month listing. The implementation uses arXiv OAI-PMH category
sets, not the human HTML listing pages.

Create a May 2026 mathematics batch:

```sh
grokrxiv batch create --category math --month 2026-05 --daily-limit 30 --auto-pr --json
```

Run due items:

```sh
grokrxiv batch run <BATCH_ID> --json
```

Track progress:

```sh
grokrxiv batch status <BATCH_ID> --json
grokrxiv batch list --json
```

For a daily schedule, run `grokrxiv batch run <BATCH_ID> --json` from cron,
launchd, GitHub Actions, or another scheduler with the repo `.env` loaded. The
batch tables record `queued`, `running`, `reviewed`, `pr_open`, `failed`, and
`skipped` item states so interrupted runs can be inspected and resumed.

## Validation

Focused checks:

```sh
cargo test -p grokrxiv-orchestrator --lib cli::tests
cargo check -p grokrxiv-orchestrator --all-targets
```

Full local review smoke:

```sh
set -a && source .env && set +a
PATH="$PWD/target/release:$PATH" grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307
```

End-to-end web/API smoke:

```sh
bash scripts/pipeline-e2e.sh 2605.17307
```

## Project Layout

```text
apps/web/                  Next.js public review UI and API proxy
crates/orchestrator/       CLI, HTTP API, supervisor, review flow, scheduler
crates/ingest/             arXiv, local file, and git source preparation
crates/render/             HTML, Markdown, TeX, PDF, and zip rendering
crates/publisher/          GitHub PR publication and moderation handoff
crates/schemas/            Shared Rust types and JSON schema generation
crates/verifier/           Deterministic and model-assisted verification
agents/*.yaml              Current role model, runner, and verifier config
prompts/*.md               Role prompt templates
schemas/*.schema.json      Closed JSON contracts for agent outputs
supabase/migrations/       Postgres schema and RLS policies
docs/                      Operator docs and implementation plans
```

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license
