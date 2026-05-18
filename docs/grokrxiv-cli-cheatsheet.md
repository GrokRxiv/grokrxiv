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

### A. Default CLI path

Each role uses the provider/model from its `agents/*.yaml`:
- summary → claude-haiku
- technical_correctness → claude-opus
- novelty → gemini-3-flash-preview
- reproducibility → gpt-5.5
- citation → gemini-3-flash-preview
- meta_reviewer → claude-sonnet-4-6

```sh
# Single paper, default review runner + default extractor = cli
grokrxiv review 2605.00228 --json

# Batch parallel (orchestrator fan-out)
grokrxiv ingest 2605.00228 2605.00316 2605.00478 --json
```

Cost: review and extraction use the local CLI auth path by default. In
`--runner cli --extractor cli` runs, GrokRxiv strips provider API key env vars
from the child `claude` / `codex` / `gemini` processes so the CLIs use their
logged-in subscription/auth state. Pass `--runner api --extractor api` only
when you intend to spend against provider API credits.

### B. Explicit runner selection

Spawns `claude -p`, `codex exec`, or `gemini -p` based on each role's YAML `provider:` field. Auth comes from `~/.claude.json`, `~/.codex/auth.json`, `~/.gemini/oauth_creds.json` (+ `~/.config/gcloud/application_default_credentials.json`) on the host.

> **B4 (FP-RPT3b) cost note:** marginal cost is per-provider, not uniformly $0. Claude Max + Pro = $0 against the subscription cap; codex on ChatGPT Plus uses the Plus tier's bundled allowance; gemini routes through your configured local CLI auth. `CliRunner` logs `event=cli_auth_path` once per provider per run so you can verify. See `feedback_cli_path_is_cost_control.md` in this operator's memory for the full audit.

```sh
# Explicit CLI (same as default; every role goes through the local CLI of its provider)
grokrxiv review 2605.00001 --runner cli --extractor cli --json

# API path, explicit because it spends provider API credits
grokrxiv review 2605.00001 --runner api --extractor api --json
```

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

## Web visibility

Public reviews are visible on the web when `visibility='public'` and
`status IN ('pr_open','published','corrected','rejected')`. `grokrxiv approve`
opens the PR and transitions the review to `pr_open`; the merge webhook later
flips it to `published`.

```sh
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

- **Gemini CLI JSON output** — GrokRxiv invokes Gemini with `-o json` and unwraps the `.response` payload before schema validation. Keep Gemini on the CLI path unless you explicitly choose API billing.
- **Codex `exec --json` streams JSONL events** — RPT2 G fix extracts the final `agent_message.text`. Should "just work" now.
- **Claude CLI skill** — invoked via `/grokrxiv-review` prepended to the prompt (not a `--skill` flag; that flag doesn't exist).
- **CLI default timeout** — 360s/role. Bump via `GROKRXIV_CLI_TIMEOUT_SECS` if a role legitimately needs longer.
- **Large bibliographies** — citation review is LLM-backed by default. If a paper legitimately needs a different model, use `GROKRXIV_CITATION_MODEL=...` or `--model-for citation=...`.

## Real E2E to verify

```sh
grokrxiv doctor --json | jq -e '.api_runners.anthropic.status == "ok"'
out=$(grokrxiv review 2605.12484 --json)
echo "$out" | jq -e '.review_id and .status == "awaiting_moderation" and (.agents | length) == 6'
rid=$(echo "$out" | jq -r .review_id)
grokrxiv approve "$rid" --json | jq -e '.pr_url | test("^https://github.com/")'
```
