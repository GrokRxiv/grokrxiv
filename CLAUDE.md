# GrokRxiv — Claude conventions

Project-specific instructions for Claude Code working in this repo. Read this before making changes.

## What this project is

GrokRxiv is an agentic AI peer-review system for arXiv papers. Rust orchestrates typed DAGs whose nodes may be Rust-native tools, CLI tools, agents, verifier/gate nodes, artifacts, or calls into other DAGs. The paper-review DAG still produces typed, JSON-schema-enforced reviews that are gated through a verifier ladder, persisted to Postgres, and (after moderation) published as PRs against a GitHub mirror of the paper.

## Architecture map

- `apps/web/` — Next.js 16 frontend (App Router, Tailwind 4, shadcn/Radix). Production UI.
- `crates/orchestrator/` — Rust supervisor that runs DAG manifests and owns side effects.
- `crates/llm-adapter/` — Multi-provider LLM client (claude / openai / gemini / vllm).
- `crates/{ingest,render,publisher,schemas,verifier}/` — pipeline stages.
- `dags/*.yaml` — DAG-type manifests: tools, roles, nodes, edges, and `dag_call` composition.
- `agents/*.yaml` — per-role config: provider, model, schema, verifiers, max_retries.
- `schemas/*.schema.json` — typed output contracts (the single source of truth).
- `prompts/*.md` — per-role prompt templates.
- `supabase/migrations/` + `migrations/` — DB schema.
- `research/` — design docs (`.md` canonical, `.html` auto-generated; see FP6).
- `~/.claude/plans/` — per-pass plan files (`fpN-*.md`); index at `piped-bubbling-brook.md`.
- `AGENTS.md` — cross-agent instructions for adding DAGs, Rust tools/functions, CLI tools, and agents.

## Hard rules

1. **Never use the user's Claude Code CLI API key for the orchestrator.** The orchestrator reads `ANTHROPIC_API_KEY` from `.env` — that is the **GrokRxiv project key**, not the user's personal CLI key. Confusing them inflates the user's CLI bill against the wrong account.
2. **`/legal` is the only page that carries the AI-disclaimer.** FP3 locked this directive: do NOT re-add the disclaimer to render artifacts, upload UI, or any other route.
3. **All schemas are OpenAI-strict-compatible.** Every property must be in `required`; nullable fields use `type: ["X", "null"]`; no `format: uri`, no `minimum/maximum`, no `minLength/maxLength`, no `minItems`. The Gemini adapter has a `sanitize_schema_for_gemini()` shim that translates from this form.
4. **Cost-aware role assignment.** Opus is reserved for `technical_correctness` (claim-by-claim audit). Other roles use Haiku / Sonnet / gpt-5.5 / gemini-2.5-flash per their `agents/*.yaml`. Don't promote a role to Opus without measuring.
5. **`meta_reviewer` input contract (FP6 fix):** receives only the 5 specialist outputs, NOT the full paper extract. The paper is already baked into specialist reasoning.
6. **New plan runs start from clean lineage.** Commit any existing dirty feature-branch work, merge it locally to `main`, revalidate `main`, then create a fresh branch before applying a new plan.
7. **Schemas, manifests, prompts, and catalogs are LLM-facing structural contracts.** They must be explicit, stable, and easy for an LLM to follow without inventing fields or breaking shape. If a contract changes, update the schema, prompt, manifest, Rust catalog/types, and tests together.

## DAG/tool/agent authoring

Follow `AGENTS.md` for the standard way to add Rust tools/functions, CLI tools,
agents, and whole DAG types. Do not add new orchestration by hardcoding a
special case into the supervisor when a manifest node, registered Rust handler,
or `dag_call` can express it.

LLM readability is a product requirement: prefer explicit names and small
contract files over implicit conventions or catch-all modules.

## How to run the M1 smoke test

```
tests/m1-pipeline.sh
```

Must pass 8/8. Exercises all 3 providers end-to-end with `verifier_status=pass` on all 6 agents.

## DB

- `papers` — arXiv papers ingested
- `paper_assets` — PRIVATE; service-role only (PDF/LaTeX paths)
- `reviews` — one row per review run; `meta_review` JSONB; status enum
- `review_agents` — provenance per specialist run; tokens, latency, verifier_status, output
- `review_inputs` (FP6) — deduped paper extract / specialist outputs; FK from `review_agents`
- `review_cache` (FP6) — per `(paper_id, role, content_hash)`; TTL 30d
- `moderation_queue` — human gate; CLI today, admin UI in FP7
- `jobs` — orchestrator task state
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
- FP6 — Pipeline cost fixes + research site + repo bootstrap (you are here)
- FP7 — Auth + user/admin consoles (planned)
- FP8 — API console + pricing + revision agents (planned)

## Style

- Don't add comments that restate what the code does.
- Don't add error handling, fallbacks, or validation for impossible scenarios — trust internal code; validate at boundaries.
- Don't widen scope on a bug fix. Three similar lines is better than a premature abstraction.
- Don't proactively create docs (`*.md`, `README.md`); only when the user asks.

## Skills available

See `SKILLS.md` for the index of `.claude/skills/`.
