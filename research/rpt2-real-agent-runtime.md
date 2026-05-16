# RPT2 — Real agent runtime (ReviewAgent + 4-runner taxonomy + grokrxiv CLI + revision mode)

> Status: Shipped, 2026-05-15. Spec phases 1–6 of `research/agent-runner.md` implemented in one parallel-agent-team mega-pass.

## What shipped

### Agent runtime (Tracks Z, A, B, C, E, H2)

- New module `crates/orchestrator/src/agents/` with:
  - `types.rs` — `AgentRunnerKind` (4 variants: `Api`/`Cli`/`Cloud`/`LocalInference`), `SandboxPolicy`, `AgentMode`, `RevisionTarget`, `AgentSpec`, `AgentInput`, `AgentRun`, `ToolPolicy`.
  - `traits.rs` — `ReviewAgent` + `AgentRunner` async traits.
  - `review_agents.rs` — 7 concrete impls (Summary, TechnicalCorrectness, Novelty, Reproducibility, Citation, MetaReviewer, RenderAgent).
  - `runners/{api,cli,cloud,local_inference}.rs` — 4 concrete runner backends.
- Supervisor refactored: `call_with_schema` deleted; `run_review_dag` delegates through `agent.run(&runner, input)`. Cache + verifier ladder + DB persist stay in supervisor (side-effect boundary preserved).
- All 6 `agents/*.yaml` now declare `runner: api` (default; safe).

### CLI runner — local subprocess (Track B)

- Spawns `claude` / `codex` / `gemini` based on `spec.provider` (claude→claude, openai→codex, gemini→gemini). No `--cli-agent` flag — the YAML's existing `provider:` drives the binary choice.
- One-shot corrective JSON-parse retry; schema validation via `jsonschema`.
- Auto-installs `~/.claude/skills/grokrxiv-review/SKILL.md` for JSON-only enforcement when claude is invoked.
- 11 unit tests pass.

### Local-inference runner — Ollama / LiteLLM (Track C)

- OpenAI-compat HTTP client. Prefers `GROKRXIV_LITELLM_URL`; falls back to `OLLAMA_HOST` (`http://localhost:11434` default) on `/v1/chat/completions`.
- `response_format: {"type":"json_object"}` + post-parse validation.
- 7 wiremock tests pass.

### Cloud runner — Vercel Open Agents primary, E2B alternate (Track E)

- Vercel path: POST `/api/run` + 2s polling on `/api/run/{id}` until `completed`/`failed`. One-shot corrective retry.
- E2B path: stubbed with clean redirect-to-Vercel error; full SDK integration is a follow-up.
- 6 wiremock tests pass.

### `grokrxiv` CLI binary + HTTP API (Track I)

- `[[bin]] name = "grokrxiv"` installs to `~/.cargo/bin/grokrxiv` via `cargo install --path crates/orchestrator --features full --bin grokrxiv --locked` (also wired into `just install`).
- New subcommand `Command::Review { source, type }` accepts arXiv id, arXiv URL, `@papers.txt` batch, `-` stdin, or local PDF/TeX path.
- 13 new global flags (`--runner`, `--runner-for`, `--sandbox`, `--cloud-provider`, `--litellm-url`, `--ollama-host`, `--model-for`, `--mode`, `--revision-target`, `--max-cost-usd`, `--no-cache`, `--offline`, `--dry-run`, `--json`, `--profile`, `--config`).
- Layered config: CLI > ENV > TOML > defaults. Profile-keyed `~/.grokrxiv/config.toml`. `grokrxiv config show [--json]`.
- `grokrxiv doctor [--json]` preflight: structured per-runner reachability report (DB, API providers, CLI binaries, cloud, local inference, publisher).
- HTTP API write endpoints under `apps/web/app/api/v1/*` with bearer-token auth (`GROKRXIV_SERVICE_TOKEN`).
- `/internal/v1/*` orchestrator routes for the apps/web proxy (stub-enqueue for RPT2; full async-job wiring is a follow-up).
- `tests/m1-pipeline.sh` rewritten as a ~15-line `grokrxiv review --json | jq -e <predicate>` script.

### Revision mode — `review_and_revise` (Track F)

- `schemas/revision_artifact.schema.json` — strict per-patch contract (target enum + patches with section/original/proposed/rationale/confidence).
- New `reviews.mode` column (default `review_only`) and `revision_patches` table with `accepted_indices[]` + `applied_pr_url`.
- `supervisor::apply_revisions(state, review_id, accepted_indices)` — DB plumbing in place; LaTeX patching itself stubbed with simulated PR URL.
- CLI flags `--mode review_and_revise --revision-target {paper_latex|grokrxiv_review_output}`.

### apps/web review-page polish (Track H1)

- `apps/web/lib/render-math.ts` — server-side `katex.renderToString` with macro injection (BASE_MACROS + future per-paper `\newcommand` extraction stub).
- `apps/web/components/markdown-body.tsx` — sanitizes via `isomorphic-dompurify`; pre-renders math; auto-slugs `<h2>/<h3>`.
- `apps/web/components/review-toc.tsx` — sticky sidebar TOC with Intersection Observer scroll-spy.
- `apps/web/app/globals.css` — full `.prose-review` typography block.
- Operator-reported issue fixed: section headings, math, and TOC all render correctly.

### Deployment topology (Track K)

- `infra/compose.local.yml` — Mac-native topology (Ollama on host via brew/`.app`; surrounding services in Docker via `host.docker.internal`).
- `infra/compose.cloud.yml` — full containerized topology with optional NVIDIA GPU passthrough for Ollama + vLLM (profile-gated).
- `infra/litellm/config.yaml` + `config.cloud.yaml` — LiteLLM gateway routes default-Ollama + frontier API pass-through.
- `Justfile` recipes: `up-local`, `up-cloud`, `down`, `install`, `serve`, `doctor`.

## Decisions locked

- **Runner taxonomy = exactly 4**: `api` / `cli` / `cloud` / `local_inference`. No `local_container` runner — container isolation is the orthogonal `SandboxPolicy::Container` (deferred to a follow-up; not shipped in RPT2 per the operator).
- **CLI agent selection comes from YAML's `provider:` field** — no `--cli-agent` runtime flag.
- **`provider:` + `model:` stay in YAML/TOML profiles** — no ENV/CLI override (only `--model-for ROLE=MODEL` for one-off experimentation).
- **Ollama is the only local-inference backend the runner directly knows** — vLLM/MLX/llama.cpp stay deployment-time choices behind LiteLLM.
- **Vercel Open Agents is the primary cloud target**; E2B is registered as an alternate.

## Verification

| Check | Result |
|---|---|
| `cargo build --workspace` | clean (1 benign warning about shared `main.rs` between 2 binary targets) |
| `cargo test --workspace --lib` | 114 / 114 pass (10 + 28 + 50 + 6 + 5 + 15 across config/llm-adapter/orchestrator/publisher/schemas/verifier) |
| `cargo install … --bin grokrxiv --locked` | succeeded; binary at `/Users/mlong/.cargo/bin/grokrxiv` |
| `grokrxiv --version` | `grokrxiv 0.1.0` |
| `grokrxiv doctor --json` | DB ok; 3 API runners ok; 3 CLI binaries detected (claude/codex/gemini); cloud + local_inference + publisher skipped (env unset) |
| `pnpm tsc --noEmit` in `apps/web` | clean |
| **REAL E2E** — `grokrxiv review 2605.12484 --no-cache --json` | **PASS**: 56s wall, 6/6 agents `verifier_status=pass`, new `review_id=820e659b-…`, status `awaiting_moderation` |

### Real E2E per-role measurement (this run, paper `2605.12484`)

| Role | Model | tokens_in | tokens_out | latency_ms |
|---|---|---|---|---|
| citation | gemini-2.5-flash | 579 | 53 | 2,378 |
| novelty | gemini-2.5-flash | 578 | 633 | 15,659 |
| summary | claude-haiku-4-5-20251001 | 864 | 582 | 6,153 |
| reproducibility | gpt-5.5 | 838 | 577 | 13,745 |
| technical_correctness | claude-opus-4-7 | 1,422 | 1,970 | 25,379 |
| meta_reviewer | claude-sonnet-4-6 | 3 | 1,216 | 23,853 |

Same envelope as RPT1 (~$0.18–0.19/paper).

## Known follow-ups (not shipping in RPT2)

1. **`--runner cli` flag-to-spec wiring** — the global flag is parsed and the RuntimeConfig captures it, but the supervisor's per-role spec is still built from YAML+profile only. To make `--runner cli` actually switch at runtime, the supervisor's resolver needs to consult `RuntimeConfig.runner` / `runner_for` and rebuild the AgentSpec on each call. ~30 minutes of work.
2. **`SandboxPolicy::Container`** — deferred per operator. Docker isolation is wired into the type system but not implemented.
3. **`apply_revisions` LaTeX patching** — DB plumbing is in place + simulated PR URL; the actual fork-and-patch loop is a follow-up.
4. **`/internal/v1/*` async-job enqueue** — currently stub-acks; full job dispatch is a follow-up.
5. **Local PDF/TeX `review` path** — `grokrxiv review ./paper.pdf` parses but returns "deferred to Track I follow-up"; arXiv id/URL/@file work end-to-end.

## Files of record

- Plan: `~/.claude/plans/rpt2-real-agent-runtime.md` (the approved plan)
- This summary: `research/rpt2-real-agent-runtime.md`
- 14 per-fix `docs/*-applied.md` writeups auto-published as HTML by the Stop hook (deployment-topology, litellm-gateway, grokrxiv-cli-reference, grokrxiv-env-reference, grokrxiv-api-reference, plus per-track applied notes if subagents wrote them)
- All `docs/*-applied.md` and `research/rpt2-*.md` files auto-mirror to `research/*.html` for the local research viewer at `localhost:3100`.
