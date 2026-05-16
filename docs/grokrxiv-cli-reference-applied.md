# `grokrxiv` CLI reference — applied

This is the operator-facing surface shipped by RPT2 Track I. The same binary
is installed under two names: `grokrxiv-orchestrator` (legacy / docker /
CI-friendly) and `grokrxiv` (ergonomic, what `just install` puts on `$PATH`).

## Installation

```sh
just install          # cargo install --path crates/orchestrator --features full --bin grokrxiv --locked
just doctor           # preflight
just serve            # blocking; runs HTTP API + supervisor + scheduler
```

## Global flags (apply to every subcommand)

| Flag                       | Default        | Notes |
|----------------------------|----------------|-------|
| `--mode <m>`               | `review_only`  | `review_only` or `review_and_revise` |
| `--revision-target <t>`    | `paper_latex`  | `paper_latex` or `grokrxiv_review_output` |
| `--runner <r>`             | _from config_  | `api` / `cli` / `cloud` / `local_inference` |
| `--extractor <r>`          | `cli`          | Staged ingest extraction backend: `cli` / `api` |
| `--runner-for <role>=<r>`  | _empty_        | Repeatable. e.g. `--runner-for summary=cli` |
| `--sandbox <s>`            | `none`         | `none` or `container` |
| `--cloud-provider <name>`  | _from env_     | e.g. `vercel_open_agents`, `e2b` |
| `--litellm-url <url>`      | _from env_     | LiteLLM gateway override |
| `--ollama-host <url>`      | _from env_     | Direct Ollama override |
| `--model-for <role>=<id>`  | _empty_        | Repeatable model override |
| `--max-cost-usd <n>`       | _none_         | Hard cap on total cost (USD) per review |
| `--no-cache`               | `false`        | Skip the review cache |
| `--offline`                | `false`        | Disallow network where avoidable |
| `--dry-run`                | `false`        | Plan-only; don't make LLM calls |
| `--json`                   | `false`        | Emit JSON (where supported) |
| `--profile <name>`         | `default`      | Named TOML profile |
| `--config <path>`          | `~/.grokrxiv/config.toml` | Override TOML path |
| `--show-secrets`           | `false`        | Print provider secrets in cleartext (`config` only) |

When the resolved runtime is CLI-only (`--runner cli --extractor cli`),
`grokrxiv` sets `GROKRXIV_ALLOW_PROVIDER_API=0` internally and removes provider
API key env vars from local CLI children. Direct provider API calls are enabled
only by explicit API selection.

## Subcommands

### Service

#### `grokrxiv serve`
Runs the orchestrator: HTTP API + supervisor + scheduler. Identical to the
`grokrxiv-orchestrator` default. Blocks forever.

#### `grokrxiv doctor`
Runs the preflight checks (env vars, DB URL, API runner reachability, CLI
binaries on PATH, cloud reachability, local-inference reachability,
publisher). Exits 1 if any *critical* check fails (DATABASE_URL absent, or
no review runner reachable). Add `--json` for a structured report.

```sh
grokrxiv doctor                       # human
grokrxiv doctor --json | jq .         # machine
```

#### `grokrxiv config [--show-secrets]`
Prints the resolved config: the env-based legacy `Config` *and* the layered
`RuntimeConfig` (defaults → TOML → env → CLI). Add `--json` for a structured
view. Secrets are redacted as `***` unless `--show-secrets` is passed.

#### `grokrxiv migrate`
Applies pending Supabase migrations (currently delegates to
`bash infra/supabase/setup.sh`; native bridge tracked under task #11).

#### `grokrxiv categories`
Prints `DEFAULT_ACTIVE_CATEGORIES` and the active env override
(`INGEST_CATEGORIES`).

### Canonical review entry point

#### `grokrxiv review <source> [--type T]`
The "single command for one paper" entry point. `source` may be:

- bare arXiv id, e.g. `2605.12484`
- arXiv URL, e.g. `https://arxiv.org/abs/2605.12484v1`
- legacy arXiv id, e.g. `math-ph/0506010`
- local PDF, e.g. `./paper.pdf`              (deferred — Track I follow-up)
- local LaTeX, e.g. `./paper.tex`            (deferred — Track I follow-up)
- `-` to read from stdin                      (deferred — Track I follow-up)
- `@<path>` to read a newline-delimited file of sources (recurses through this list)

With `--json`, after the run we emit:

```json
{
  "arxiv_id": "2605.12484",
  "review_id": "8f9e...",
  "status": "awaiting_moderation",
  "agents": [
    {"role": "summary", "verifier_status": "pass"},
    {"role": "technical_correctness", "verifier_status": "pass"},
    ...
  ]
}
```

The smoke test in `tests/m1-pipeline.sh` asserts on this envelope.

### Ingestion (lower-level)

#### `grokrxiv ingest <arxiv_id>...`
Synchronously ingest + run the review DAG on one or more papers. Single
paper prints `arxiv_id=… review_id=…`; multiple papers fan out in parallel.

#### `grokrxiv ingest-range --from D --to D [--categories C,C,C] [--no-review]`
Bulk OAI-PMH backfill across a date range.

#### `grokrxiv ingest-daily`
Equivalent of the daily scheduler tick (yesterday → today).

### Review lifecycle

#### `grokrxiv list reviews [--status S] [--field F] [--limit N] [--json]`
#### `grokrxiv list papers [--field F] [--has-review] [--limit N] [--json]`

#### `grokrxiv show <REVIEW_ID> [--json]`
Pretty-print a review (paper, agents, verifier status, optional PR URL).

#### `grokrxiv re-review <PAPER_ID>`
Re-run the review DAG against an already-ingested paper (renamed from `review`).

#### `grokrxiv verify <REVIEW_ID>`
Re-print the per-agent verifier_status rows for a review.

#### `grokrxiv render <REVIEW_ID> [--format html|md|tex|pdf|zip] [--out PATH]`
Re-emit artifacts for a persisted review.

### Moderation

#### `grokrxiv approve <REVIEW_ID> [--json]`
Open the publication PR on `GrokRxiv/reviews`. Prints `pr_url=…`. With
`--json`, returns `{review_id, pr_url, status}`. Without `GITHUB_TOKEN`, the
PR is simulated.

#### `grokrxiv reject <REVIEW_ID> --reason TEXT`
Mark a review rejected (review stays `awaiting_moderation`).

#### `grokrxiv request-changes <REVIEW_ID> --notes TEXT`
Mark a review as needing operator changes.

#### `grokrxiv withdraw <REVIEW_ID> --reason TEXT`
Withdraw a published review (status → `withdrawn`; revalidates the
frontend).

#### `grokrxiv correct <REVIEW_ID> --rationale-md PATH`
Append a correction; status → `corrected`.

### Conveniences

#### `grokrxiv open <REVIEW_ID>`
Print (and on macOS, `open(1)`) the `/reviews/<id>` URL.

#### `grokrxiv tail-jobs [--kind K] [--state S]`
Stream the jobs table tail. (Wiring tracked under task #15.)

## Examples

```sh
# Full M1 smoke
grokrxiv review 2605.12484 --json

# Force the technical-correctness role to run via the local Claude CLI:
grokrxiv review 2605.12484 \
    --runner-for technical_correctness=cli \
    --json

# Switch the entire run to local-inference + cap cost:
grokrxiv review 2605.12484 \
    --runner local_inference \
    --litellm-url http://localhost:4000 \
    --max-cost-usd 0.10 \
    --json

# Read sources from a manifest:
grokrxiv review @sources.txt --json | jq -s 'map(.review_id)'
```

## Cross-references

- Env vars: `docs/grokrxiv-env-reference-applied.md`
- HTTP API: `docs/grokrxiv-api-reference-applied.md`
- Config TOML: `docs/grokrxiv-config.example.toml`
