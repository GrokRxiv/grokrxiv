# GrokRxiv

Agentic AI peer-review system for arXiv papers.

> **Status:** FP5 shipped (architecture-of-record for hybrid local + subscription deployment). FP6 in progress (cost fixes, research site, repo bootstrap).

## What it does

GrokRxiv ingests an arXiv paper, runs a typed multi-agent DAG of six specialist reviewers — `summary`, `technical_correctness`, `novelty`, `reproducibility`, `citation`, `meta_reviewer` — each producing JSON-schema-validated output, gates the result through a composite verifier ladder, and (after human moderation) publishes the review as a PR against a GitHub mirror of the paper.

The contract is the JSON schema, not the model. Backends are interchangeable: frontier APIs (Claude / OpenAI / Gemini), self-hosted OSS (Qwen-2.5 / DeepSeek / Llama via vLLM or Ollama), or operator-owned subscription tiers (Claude Code + Codex CLI shims).

## Quickstart

Prerequisites: `pnpm`, `cargo` (Rust ≥1.80), `supabase` CLI, Docker, and
Pandoc for local TeX extraction. The orchestrator Docker image installs Pandoc
by default.

```sh
# 1. Install JS deps + start Supabase locally
pnpm install
supabase start

# 2. Build the Rust workspace
cargo build --workspace

# 3. Set provider keys (project-specific keys, NOT your personal CLI key)
cp .env.example .env
$EDITOR .env   # ANTHROPIC_API_KEY, OPENAI_API_KEY, GOOGLE_GENERATIVE_AI_API_KEY

# 4. Run the M1 end-to-end smoke
bash tests/m1-pipeline.sh

# 5. Serve the production frontend
cd apps/web && pnpm dev   # http://localhost:3000

# 6. (Optional) Serve the local research viewer
cd research/site && pnpm install && pnpm dev   # http://localhost:3100
```

## Project layout

```
apps/web/                  Next.js 16 production frontend
crates/orchestrator/       Rust supervisor — runs the 6-agent DAG
crates/llm-adapter/        Multi-provider client (claude/openai/gemini/vllm)
crates/{ingest,render,publisher,schemas,verifier}/   pipeline stages
agents/*.yaml              per-role config (provider, model, schema, verifiers)
schemas/*.schema.json      typed output contracts (the single source of truth)
prompts/*.md               per-role prompt templates
supabase/migrations/       database schema
tests/m1-pipeline.sh       end-to-end smoke test
research/                  design docs (markdown sources, HTML artifacts)
research/site/             local-only Next.js viewer for research/
.claude/                   project Claude conventions, settings, hooks, skills
CLAUDE.md                  Claude conventions for this repo (read first)
SKILLS.md                  index of available skills
```

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — Claude conventions, hard rules, plan history
- [`SKILLS.md`](./SKILLS.md) — index of project skills
- [`research/`](./research/) — architecture docs (open the `.html` files in any browser, or run the viewer)

## License

TBD.
