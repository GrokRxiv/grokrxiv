# `grokrxiv` CLI cheatsheet

Quick reference for running the agentic peer-review pipeline from the command line.

## One-time setup

```sh
# Install the binary (from the repo root)
just install
# or
cargo install --path crates/orchestrator --features full --bin grokrxiv --locked

# Verify
grokrxiv --version            # grokrxiv 0.1.0
grokrxiv doctor               # preflight per-runner reachability check
grokrxiv doctor --json        # machine-readable
```

## Required environment

These come from `.env` at the repo root. The CLI sources them automatically when run from the repo, but for ad-hoc shells:

```sh
cd /Users/mlong/Documents/Development/grokrxiv
set -a; source .env; set +a
export DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:54322/postgres"
export GITHUB_TOKEN="$(gh auth token)"
export GROKRXIV_REVIEWS_OWNER="GrokRxiv"
export GROKRXIV_REVIEWS_REPO="grokrxiv-reviews"
```

The CLI must be run from a directory that can find `agents/*.yaml` (currently the repo root). Future polish will resolve them by absolute path.

## Two-runner test patterns

### A. API path (paid provider APIs)

Each role uses the provider/model from its `agents/*.yaml`:
- summary → claude-haiku
- technical_correctness → claude-opus
- novelty → gemini-2.5-pro
- reproducibility → gpt-5.5
- citation → gemini-2.5-pro
- meta_reviewer → claude-sonnet-4-6

```sh
# Single paper, default runner = api
grokrxiv review 2605.00228 --json

# Batch parallel (orchestrator fan-out)
grokrxiv ingest 2605.00228 2605.00316 2605.00478 --json
```

Cost: ~$0.18–0.22 per paper (Tier-2 Anthropic + OpenAI + paid Gemini Pro).

### B. CLI path (local subscriptions: claude / codex / gemini)

Spawns `claude -p`, `codex exec`, or `gemini -p` based on each role's YAML `provider:` field. Auth comes from `~/.claude`, `~/.codex`, `~/.config/gemini` on the host. Zero API spend; uses your existing subscriptions.

```sh
# Pure CLI (every role goes through the local CLI of its provider)
grokrxiv review 2605.00001 --runner cli --json

# Hybrid: CLI for claude/codex roles, API for gemini roles
# (gemini CLI doesn't honor strict-schema output reliably — known issue)
grokrxiv review 2605.00001 \
  --runner cli \
  --runner-for novelty=api \
  --runner-for citation=api \
  --json
```

The hybrid pattern is the practical recommendation today.

### C. Forcing per-role overrides

```sh
# Run everything via API but route the expensive technical_correctness
# through your Claude Code subscription instead
grokrxiv ingest 2605.12484 \
  --runner-for technical_correctness=cli \
  --json

# Run cheap roles on local OSS (requires Ollama running)
grokrxiv ingest 2605.12484 \
  --runner-for summary=local_inference \
  --runner-for citation=local_inference \
  --json
```

## Approve → real GitHub PR

```sh
# After ingest completes, approve to open a real PR on GrokRxiv/grokrxiv-reviews
grokrxiv approve <REVIEW_UUID> --json
# Returns: {"pr_url":"https://github.com/GrokRxiv/grokrxiv-reviews/pull/N", ...}
```

## Make it visible on the web

The Supabase RLS gate restricts anon visibility to `status IN ('published','corrected')`. Until the PR-merge webhook is wired, transition manually:

```sh
docker exec supabase_db_grokrxiv psql -U postgres -d postgres -c "
UPDATE reviews SET status='published', published_at=now()
WHERE id = '<REVIEW_UUID>';
"

# Verify:
curl -sf -o /dev/null -w "%{http_code}\n" \
  http://localhost:3000/reviews/<REVIEW_UUID>
```

## Useful flags

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output |
| `--dry-run` | Print resolved plan; no LLM calls |
| `--no-cache` | Skip `review_cache` hits this run |
| `--profile <name>` | Load profile from `~/.grokrxiv/config.toml` |
| `--max-cost-usd 0.50` | Per-paper ceiling; fails fast on overrun |
| `--model-for ROLE=MODEL` | Swap model for one role this run |
| `--mode review_and_revise` | Emit revision patches (Phase 6 mode) |
| `--revision-target {paper_latex,grokrxiv_review_output}` | Where revisions land |

## Doctor — preflight

```sh
grokrxiv doctor          # human-readable
grokrxiv doctor --json   # CI-friendly
```

Outputs reachability per runner:
- `api` — anthropic / openai / gemini key validation
- `cli` — `claude` / `codex` / `gemini` binaries on PATH + auth
- `cloud` — VERCEL_OPEN_AGENTS_URL / E2B_API_KEY
- `local_inference` — OLLAMA_HOST / GROKRXIV_LITELLM_URL

## Known limitations (RPT2 ship)

- **Gemini CLI doesn't honor JSON schemas reliably** — `gemini -p` invents extra fields or emits the schema document. Recommend `--runner-for novelty=api --runner-for citation=api` when using `--runner cli`.
- **Codex `exec --json` streams JSONL events** — RPT2 G fix extracts the final `agent_message.text`. Should "just work" now.
- **Claude CLI skill** — invoked via `/grokrxiv-review` prepended to the prompt (not a `--skill` flag; that flag doesn't exist).
- **CLI default timeout** — 360s/role. Bump via `GROKRXIV_CLI_TIMEOUT_SECS` if a role legitimately needs longer.
- **Large bibliographies via gpt-5.5** — citation role can truncate at `max_tokens: 6000` for math papers with 30+ refs. Workaround: route citation to `--runner-for citation=cli` (codex handles long output better) or bump max_tokens in `agents/citation.yaml`.

## Real E2E to verify

```sh
grokrxiv doctor --json | jq -e '.api_runners.anthropic.status == "ok"'
out=$(grokrxiv review 2605.12484 --json)
echo "$out" | jq -e '.review_id and .status == "awaiting_moderation" and (.agents | length) == 6'
rid=$(echo "$out" | jq -r .review_id)
grokrxiv approve "$rid" --json | jq -e '.pr_url | test("^https://github.com/")'
```
