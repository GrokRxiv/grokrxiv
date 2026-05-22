# AgentHero / GrokRxiv — Claude conventions

Project-specific instructions for Claude Code working in this repo. Read this before making changes.

## What this project is

AgentHero is the Rust/Tokio DAGOps runtime and distributed control plane.
Tokio is the async substrate for local tasks, networking, timers, timeouts,
channels, and worker I/O; AgentHero owns the app/DAG/node/artifact/capability
contracts. GrokRxiv is the first scaled DAG app proving the abstraction through
paper ingest/extract/review/revise/publish.

## Architecture map

- `apps/web/` — Next.js 16 frontend (App Router, Tailwind 4, shadcn/Radix). Production UI.
- `crates/dag-runtime/` — DAG manifest parsing and validation.
- `crates/dag-executor/` — generic manifest-driven DAG executor. It must stay free of paper/review/arXiv dependencies.
- `crates/dag-app-*` — concrete DAG apps that adapt manifests to domain tools, agents, verifiers, renderers, and publishers.
- `crates/orchestrator/` — HTTP/CLI/scheduler glue, DB/job ownership, and DAG app registry.
- `crates/llm-adapter/` — Multi-provider LLM client (claude / openai / gemini / vllm).
- `crates/{ingest,render,publisher,schemas,verifier}/` — domain tool/provider crates used by DAG apps.
- `dags/*.yaml` — DAG-type manifests: tools, roles, nodes, edges, and `dag_call` composition.
- `agents/<dag-type>/*.yaml` — per-role config: provider, model, runner, prompt template, schemas, verifier names, prompt context, overlays, postprocessors, max_retries.
- `schemas/*.schema.json` — typed output contracts (the single source of truth).
- `prompts/*.md` — per-role prompt templates.
- `supabase/migrations/` + `migrations/` — DB schema.
- `research/` — design docs (`.md` canonical, `.html` auto-generated; see FP6).
- `~/.claude/plans/` — per-pass plan files (`fpN-*.md`); index at `piped-bubbling-brook.md`.
- `AGENTS.md` — cross-agent instructions for adding DAGs, Rust tools/functions, CLI tools, and agents.

## Hard rules

1. **Never use the user's Claude Code CLI API key for the orchestrator.** The orchestrator reads `ANTHROPIC_API_KEY` from `.env` — that is the **GrokRxiv project key**, not the user's personal CLI key. Confusing them inflates the user's CLI bill against the wrong account.
2. **`/legal` is the only page that carries the AI-disclaimer.** FP3 locked this directive: do NOT re-add the disclaimer to render artifacts, upload UI, or any other route.
3. **All schemas are OpenAI-strict-compatible.** Every property must be in `required`; nullable fields use `type: ["X", "null"]`; no `format: uri`, no `minimum/maximum`, no `minLength/maxLength`, no `minItems`. The Gemini adapter has a `sanitize_schema_for_gemini()` shim that migrates from this form.
4. **Cost-aware role assignment.** Model choice belongs in `agents/<dag-type>/*.yaml` or explicit runtime overrides. Don't promote a role to a more expensive model without measuring.
5. **`meta_reviewer` input contract (FP6 fix):** receives only the 5 specialist outputs, NOT the full paper extract. The paper is already baked into specialist reasoning.
6. **New plan runs start from clean lineage.** Commit any existing dirty feature-branch work, merge it locally to `main`, revalidate `main`, then create a fresh branch before applying a new plan.
7. **Schemas, manifests, prompts, and catalogs are LLM-facing structural contracts.** They must be explicit, stable, and easy for an LLM to follow without inventing fields or breaking shape. If a contract changes, update the schema, prompt, manifest, Rust catalog/types, and tests together.
8. **Do not add app-specific Rust role enums.** Agent identity is a YAML/string DAG contract. Add reusable Rust hooks/tools, then declare them from YAML.

## DAG/tool/agent authoring

Follow `AGENTS.md` for the standard way to add Rust tools/functions, CLI tools,
agents, and whole DAG types. Do not add new orchestration by hardcoding a
special case into the supervisor when a manifest node, registered Rust handler,
or `dag_call` can express it.

Use `agh dag run --dag-type <dag> --json` for executor-path smoke tests.
The c2rust DAG app is the required non-paper proof path for generic DAG
changes.

The operator CLI is app-scoped. Use `agh app run grokrxiv -- review ...`,
`agh app run grokrxiv -- approve ...`, and
`agh app run c2rust -- migrate ...`; do not add new GrokRxiv lifecycle
commands at the root.

LLM readability is a product requirement: prefer explicit names and small
contract files over implicit conventions or catch-all modules. Reusable
behavior belongs in named hooks such as `prompt_context`, `system_overlays`,
`verifiers`, `postprocessors`, or registered tools, not hidden role-name
matches.

## How to run the M1 smoke test

```
tests/m1-pipeline.sh
```

Must pass 8/8. Exercises all 3 providers end-to-end with `verifier_status=pass` on all 6 agents.

## DB

- `app_runs` — product app action run records.
- `dag_runs` — manifest execution records under an app run.
- `dag_run_nodes` — node attempts, state, runner/model/tool/role identity, and output JSON.
- `dag_artifacts` — named artifact references produced by app/DAG/node runs.
- `dag_events` — runtime event stream.
- `worker_nodes` / `worker_leases` — distributed runner presence and work leases.
- `agent_output_cache` — generic app/DAG/node/role cache.
- `grokrxiv_sources`, `grokrxiv_reviews`, `grokrxiv_moderation_queue` — GrokRxiv app projection tables for product queries and moderation UI.
- Existing `papers`, `reviews`, `review_agents`, `review_inputs`, `review_cache`, `moderation_queue`, and `jobs` tables are migration-era GrokRxiv data/projections, not the generic DAG runtime contract.
- `uploads` — anonymous landing-page samples

## Provider keys

`.env` (gitignored) holds:
- `ANTHROPIC_API_KEY` — GrokRxiv project key (NOT the user's CLI key)
- `OPENAI_API_KEY`
- `GOOGLE_GENERATIVE_AI_API_KEY`
- `PREVIEW_MODEL` — model used for landing-page previews

`.env.example` is committed; `.env` is not.

## Plan convention (FP nomenclature)

Each significant change is a fix pass (`fpN`). Plan files live in `~/.claude/plans/`. The current pass is written to `piped-bubbling-brook.md` while plan mode is active; on ship the agent copies it to `fpN-<slug>.md` and restores the index in `piped-bubbling-brook.md`.

History:
- FP1 — Monorepo scaffold
- FP2 — Honest end-to-end with persistence + moderation
- FP3 — Codex audit follow-through (the `/legal` disclaimer rule comes from here)
- FP4 — Real typed DAG, parallel 5 specialists + meta_reviewer
- FP5 — Processing-costs architecture-of-record (design only — no code)
- FP6 — Pipeline cost fixes + GrokRxiv site + repo bootstrap (you are here)
- FP7 — Auth + user/admin consoles (planned)
- FP8 — API console + pricing + revision agents (planned)

## Style

- Don't add comments that restate what the code does.
- Don't add error handling, fallbacks, or validation for impossible scenarios — trust internal code; validate at boundaries.
- Don't widen scope on a bug fix. Three similar lines is better than a premature abstraction.
- Don't proactively create docs (`*.md`, `README.md`); only when the user asks.

## Skills available

See `SKILLS.md` for the index of `.claude/skills/`.
