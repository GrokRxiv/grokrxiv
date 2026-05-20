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

Bare `grokrxiv` prints help and exits. Start the long-running service
explicitly with `grokrxiv serve`.

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
| `--status`                 | auto           | Emit short progress lines to stderr |
| `--no-status`              | `false`        | Suppress progress lines for background runs |
| `--profile <name>`         | `default`      | Named TOML profile |
| `--config <path>`          | `~/.grokrxiv/config.toml` | Override TOML path |
| `--show-secrets`           | `false`        | Print provider secrets in cleartext (`config` only) |

When the resolved runtime is CLI-only (`--runner cli --extractor cli`),
`grokrxiv` sets `GROKRXIV_ALLOW_PROVIDER_API=0` internally and removes provider
API key env vars from local CLI children. Direct provider API calls are enabled
only by explicit API selection.

Docker runs the same CLI path. The orchestrator image installs `claude`,
`codex`, and `gemini`; compose mounts local CLI auth read-only and the
entrypoint copies only those auth bundles into `/home/grokrxiv` at startup.
Hosted deploys should provide the same files as runtime secrets, not baked
image layers.

On macOS, Claude Code stores the usable OAuth credential in Keychain, not just
`~/.claude.json`. Export that one item before starting Docker:

```sh
security find-generic-password -s 'Claude Code-credentials' -w \
  > ~/.claude/docker-claude-code-credentials.secret
chmod 600 ~/.claude/docker-claude-code-credentials.secret
```

The compose mount exposes that file read-only, and the container entrypoint
copies it to Claude Code's Linux credentials-file locations. Do not commit or
print this file. Codex and Gemini are file-backed on this machine; compose
mounts only `~/.codex/auth.json`, `~/.gemini/oauth_creds.json`, and
`~/.gemini/google_accounts.json`. The entrypoint writes a minimal Gemini OAuth
settings file inside the container so host MCP/trust settings are not copied.

Review specialists run in parallel by default. Set
`GROKRXIV_REVIEW_CONCURRENCY=1` for serial debugging, or set another positive
integer to cap concurrent specialist CLI/API children.

## Subcommands

### Service

#### `grokrxiv serve`
Runs the orchestrator: HTTP API + supervisor + scheduler. Blocks forever.

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

### Canonical review entry point

#### `grokrxiv review <source> [--type T]`
The "single command for one paper" entry point. `source` may be:

- bare arXiv id, e.g. `2605.12484`
- arXiv URL, e.g. `https://arxiv.org/abs/2605.12484v1`
- legacy arXiv id, e.g. `math-ph/0506010`
- local PDF, e.g. `./paper.pdf`              (deferred — Track I follow-up)
- local LaTeX, e.g. `./paper.tex`            (deferred — Track I follow-up)
- git repository, e.g. `https://github.com/org/repo --type git --paper-path paper.tex`
- git corpus, e.g. `https://github.com/org/repo --type git --corpus --scan-root papers`
- `-` to read from stdin                      (deferred — Track I follow-up)
- `@<path>` to read a newline-delimited file of sources (recurses through this list)

For corpus review, GrokRxiv scans for `.tex` and `.pdf`, de-duplicates
matching TeX/PDF pairs, prefers TeX, and creates one review per manuscript:

```sh
grokrxiv --runner cli --extractor cli --status --no-cache \
  review https://github.com/MagnetonIO/emergent_spacetime \
  --type git --rev main --corpus --scan-root papers/information-theory/src
```

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

### Ingestion

#### `grokrxiv extract <arxiv_id>...`
Run fetch + extraction only, then audit the reviewer input artifacts. This is
the fast standalone check for the extractor before running reviewers:

```sh
grokrxiv --runner cli --extractor cli --status --no-cache extract 2605.00561
```

The default TeX conversion is Pandoc. Local installs find `pandoc` on PATH or
via `GROKRXIV_PANDOC_BIN`; the orchestrator Docker image installs official
Pandoc by default unless built with `GROKRXIV_DOCKER_INSTALL_PANDOC=0`.
LaTeXML semantic AST enrichment runs only when
`GROKRXIV_TEX_ENABLE_LATEXML=1`; extraction LLM tool loops run only when
`GROKRXIV_FORCE_AGENT_EXTRACTION=1`.

#### `grokrxiv review-extracted [--force] <arxiv_id|paper_id|url>`
Run only the review DAG for a paper whose extraction artifacts are already
persisted. This reuses `review_input.json` and avoids re-running extraction:

```sh
grokrxiv --runner cli --extractor cli --status --no-cache review-extracted 2605.00561
```

If the paper already has an active review (`awaiting_moderation`, `pr_open`,
`published`, etc.), the command does not start a new DAG by default. It prints
`already_reviewed=true`, the existing `review_id`, current status, and PR URL
when available. With `--json`, it emits `{ "status": "already_reviewed", ... }`.
Use `--force` only when you intend to supersede the existing review after new
review input, comments, or paper/extraction changes.

### Review lifecycle

#### `grokrxiv list reviews [--review-status S] [--field F] [--limit N] [--json]`
#### `grokrxiv list papers [--field F] [--has-review] [--extracted] [--limit N] [--json]`
#### `grokrxiv list extracted [--field F] [--limit N] [--json]`

#### `grokrxiv show <REVIEW_ID> [--json]`
Pretty-print a review (paper, agents, verifier status, optional PR URL).

### Moderation

#### `grokrxiv approve <REVIEW_ID> [--json]`
Open the publication PR on `GrokRxiv/grokrxiv-reviews`. This does not merge or
publish the review; a human merge plus the GitHub webhook performs the
`published` transition. Prints `pr_url=…`. With `--json`, returns
`{review_id, pr_url, status}`. Without `GITHUB_TOKEN`, the command fails closed
instead of writing simulated PR state.

#### `grokrxiv publish <REVIEW_ID> [--json]`
Publish a review by merging its open publication PR. The GitHub webhook then
flips the review to `published` and revalidates the public site. `merge` is
kept as a hidden compatibility alias for this command.

#### `grokrxiv reject <REVIEW_ID> --reason TEXT`
Mark a review rejected (review stays `awaiting_moderation`).

#### `grokrxiv request-changes <REVIEW_ID> --notes TEXT`
Mark a review as needing operator changes.

### Conveniences

#### `grokrxiv open <REVIEW_ID>`
Print (and on macOS, `open(1)`) the `/reviews/<id>` URL.

### Advanced and hidden commands

These remain callable for compatibility and admin repair work, but are hidden
from default `--help`.

#### `grokrxiv ingest <arxiv_id>...`
Lower-level arXiv-only ingest + review path. Prefer `grokrxiv review`.

#### `grokrxiv re-review <PAPER_ID>`
Re-run the review DAG against an already-ingested paper. Prefer
`grokrxiv review-extracted --force` for operator reruns.

#### `grokrxiv verify <REVIEW_ID>`
Re-print the per-agent verifier_status rows for a review.

#### `grokrxiv render <REVIEW_ID> [--format html|md|tex|pdf|zip] [--out PATH]`
Re-emit artifacts for a persisted review.

#### `grokrxiv correct <REVIEW_ID> --rationale-md PATH`
Append a correction; status → `corrected`.

#### `grokrxiv withdraw <REVIEW_ID> --reason TEXT`
Withdraw a published review (status → `withdrawn`; revalidates the frontend).

#### `grokrxiv ingest-range --from D --to D [--categories C,C,C] [--no-review]`
Bulk OAI-PMH backfill across a date range.

#### `grokrxiv ingest-daily`
Equivalent of the daily scheduler tick (yesterday → today).

#### `grokrxiv migrate`
Stub: points operators to `bash infra/supabase/setup.sh` until native migration
wiring lands.

#### `grokrxiv categories`
Prints `DEFAULT_ACTIVE_CATEGORIES` and the active env override.

#### `grokrxiv html-review [<REVIEW_ID>|--all]`
Internal post-render formatting harness.

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
