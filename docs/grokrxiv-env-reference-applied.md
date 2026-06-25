# `agh` env-var reference — applied

Env vars consumed by the AgentHero orchestrator binary (`agh` / `agenthero`) and
the Next.js web tier. Layered config order is: CLI flags > process ENV / root
`.env` / included `.env_*` files > TOML profile > built-in defaults. The CLI's
`--profile <name>` and `--config <path>` flags pick the TOML file/profile that
ENV then overrides.

## Env Files

The root `.env` is now a selector. It should normally contain only
`AGENTHERO_ENV_FILES`, for example:

```sh
AGENTHERO_ENV_FILES=agenthero/apps/grokrxiv/env/.env_core,agenthero/apps/grokrxiv/env/.env_ingest,agenthero/apps/grokrxiv/env/.env_extract,agenthero/apps/grokrxiv/env/.env_review,agenthero/apps/grokrxiv/env/.env_publish,agenthero/apps/grokrxiv/env/.env_web,agenthero/apps/grokrxiv/env/.env_billing,agenthero/apps/grokrxiv/env/.env_dev
```

The Rust CLI/orchestrator loads `.env` first, then loads the files named in
`AGENTHERO_ENV_FILES` relative to the root `.env` directory. Existing process
vars and root `.env` values win over included files.

Docker Compose does not follow `AGENTHERO_ENV_FILES` during `${...}`
interpolation. Before `docker compose up`, export the split env files into the
shell:

```sh
set -a
source .env
for file in ${AGENTHERO_ENV_FILES//,/ }; do source "$file"; done
set +a
```

| File | Purpose |
|------|---------|
| `.env_core` | Supabase/Postgres, orchestrator bind URLs, service/admin tokens, public base URLs |
| `.env_ingest` | arXiv/Crossref endpoints, data repo paths, storage, ingest scheduler and cache controls |
| `.env_extract` | Pandoc, LaTeXML, extraction mode, and extraction-agent toggles |
| `.env_review` | LLM provider keys, AgentHero runner controls, optional role model/timeouts, review/verifier controls |
| `.env_publish` | GitHub publisher, webhook/revalidate secrets, publish-path E2E controls |
| `.env_web` | Next.js public env, web Supabase URLs, admin seed |
| `.env_billing` | Stripe billing keys and billing enablement |
| `.env_dev` | Docker platform/build toggles, diagnostics, supervisor sizing, local safety switches |

Templates are committed as `.env_<purpose>.example`. Real `.env_<purpose>`
files are gitignored.

## Service

| Env                            | Default                                      | Notes |
|--------------------------------|----------------------------------------------|-------|
| `ORCHESTRATOR_BIND`            | `0.0.0.0:8080`                              | axum bind address |
| `DATABASE_URL`                 | _unset_                                      | Required for persistence; otherwise stateless |
| `ARXIV_USER_AGENT`             | `GrokRxiv/0.1 (mailto:mlong168@gmail.com)`  | User-Agent string for arXiv |
| `GROKRXIV_PREVIEW_PROVIDER`    | `gemini`                                     | CLI provider used by homepage sample previews |
| `GROKRXIV_PREVIEW_MODEL`       | `gemini-3-flash-preview`                    | Model used by homepage sample previews; legacy `PREVIEW_MODEL` is still accepted by the binary |
| `GROKRXIV_PREVIEW_TIMEOUT_SECS` | `120`                                       | Single-pass sample preview timeout |
| `ADMIN_TOKEN`                  | _unset_                                      | Bearer for `/ingest` admin endpoint |
| `GITHUB_WEBHOOK_SECRET`        | _unset_                                      | HMAC secret for `/webhook/github` |
| `WEB_REVALIDATE_URL`           | _unset_                                      | Frontend revalidate endpoint |
| `REVALIDATE_SECRET`            | _unset_                                      | Bearer for the revalidate endpoint |
| `AGENTHERO_DOCTOR_WEB_TIMEOUT_SECS` | `3`                                    | Timeout for `agh doctor` revalidate endpoint reachability probe |

## Refresh and render quality

| Env                            | Default                                      | Notes |
|--------------------------------|----------------------------------------------|-------|
| `AGENTHERO_REFRESH_STAGE_TIMEOUT_SECS` | `15`                                | Per-stage timeout for refresh-review web revalidate and GitHub feedback updates |
| `AGENTHERO_REFRESH_RENDER_TIMEOUT_SECS` | `GROKRXIV_HTML_QUALITY_TIMEOUT_SECS + 30` | Timeout for refresh-review render plus HTML quality cleanup |
| `GROKRXIV_HTML_QUALITY_DISABLE` | _unset_                                     | Set `1`/`true` to skip HTML/PR text cleanup; leave unset for normal refresh-review behavior |
| `GROKRXIV_HTML_QUALITY_MODEL`   | `gpt-5.5`                                  | Model used by HTML quality and PR text cleanup agents |
| `GROKRXIV_HTML_QUALITY_TIMEOUT_SECS` | `180`                                | HTML quality cleanup timeout; PR text cleanup uses 120 seconds when unset |

## Layered runtime (Track I)

| Env                            | CLI equivalent              | Notes |
|--------------------------------|-----------------------------|-------|
| `AGENTHERO_RUNNER`              | `--runner`                  | `api` / `cli` |
| `AGENTHERO_EXTRACTOR`           | `--extractor`               | Staged ingest extraction backend: `cli` / `api`; default `cli` |
| `AGENTHERO_SANDBOX`             | `--sandbox`                 | `none` / `container` |
| `AGENTHERO_MODE`                | `--mode`                    | `review_only` / `review_and_revise` |
| `AGENTHERO_MAX_COST_USD`        | `--max-cost-usd`            | Hard ceiling per review |
| `GROKRXIV_FREE_REVIEW_LIMIT`   | _none_                      | Lifetime free full-review cap per logged-in user; default `3` |
| `GROKRXIV_NO_CACHE`            | `--no-cache`                | `1`/`true` to enable |
| `AGENTHERO_OFFLINE`             | `--offline`                 | `1`/`true` to enable |
| `AGENTHERO_ALLOW_PROVIDER_API`  | _internal_                  | Set by `agh`: `1` only when `--runner api`, `--extractor api`, or a per-role API override is selected |
| `AGENTHERO_SERVICE_TOKEN`       | _none_                      | Operator token for non-public web proxy routes; public `/api/v1` is read-only |
| `AGENTHERO_APPS_ROOT`           | _none_                      | Override installed `agenthero/apps` root, mainly for packaged/container runtimes |
| `AGENTHERO_AGENTS_DIR`          | _none_                      | Override app agent config directory, mainly for packaged/container runtimes |
| `AGENTHERO_DAGS_DIR`            | _none_                      | Override app DAG manifest directory, mainly for packaged/container runtimes |
| `GROKRXIV_<ROLE>_PROVIDER`      | YAML default                | Optional role provider override; same role as `--provider-for <role>=...`; useful for switching to Gemini/agy backup when desired |
| `GROKRXIV_<ROLE>_MODEL`         | YAML default                | Optional role model override; same role as `--model-for <role>=...` |
| `GROKRXIV_<ROLE>_TIMEOUT_SECS`  | YAML default                | Optional CLI subprocess timeout override for one role |
| `AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER` | _unset_          | Optional local CLI quota fallback provider. Set to `gemini` only when you want Antigravity/agy as the backup route after the primary CLI reports quota |
| `AGENTHERO_CLI_QUOTA_FALLBACK_MODEL` | `Gemini 3.5 Flash (Medium)` when fallback provider is `gemini` | Optional fallback model. Override to another current Antigravity model name when needed |
| `GROKRXIV_CITATION_PROMPT_MAX_BIB_ENTRIES` | `32`             | Maximum bibliography entries included in the Citation LLM relevance prompt; full bibliography still stays in artifacts/verifier data |
| `GROKRXIV_FORMALIZE_SOURCE_CONTEXT_MAX_BLOCKS` | `240`          | Maximum TeX theorem-inventory blocks mirrored into the local source context file for fallback tool-loop diagnostics |
| `GROKRXIV_FORMALIZE_SOURCE_CONTEXT_MAX_CHARS` | `500000`        | Character budget for the local source context file; does not cap `theorem_inventory.json` |
| `GROKRXIV_FORMALIZE_TRANSCRIPTION_BATCH_ITEMS` | `1`            | Maximum theorem-level inventory items sent to one typed-transcription LLM batch; keep this small because Sonnet typed-IR is advisory and per-item bounded |
| `GROKRXIV_FORMALIZE_TRANSCRIPTION_BATCH_CHARS` | `30000`        | Character budget for one typed-transcription LLM batch |
| `GROKRXIV_FORMALIZE_SOURCE_EXTRACTION_TIMEOUT_SECS` | `1800`    | Timeout floor for source-inventory typed transcription calls |
| `GROKRXIV_FORMALIZE_TYPED_IR_PROVIDER` | `claude`                | Provider key for typed-IR transcription; set to `gemini` only to use agy as a backup route |
| `GROKRXIV_FORMALIZE_TYPED_IR_MODEL` | `claude-sonnet-4-6`     | Model passed to the selected typed-IR provider |
| `GROKRXIV_FORMALIZE_TYPED_IR_TIMEOUT_SECS` | `120`              | Per-batch timeout floor for typed-IR transcription; failed or stale model output is recorded as a failure, not converted to fake math IR |
| `GROKRXIV_FORMALIZE_TYPED_IR_TIMEOUT_MAX_SECS` | `1800`        | Adaptive timeout ceiling for typed-IR Sonnet batches when recent successful benchmarks show longer latency |
| `GROKRXIV_FORMALIZE_TYPED_IR_BATCH_CONCURRENCY` | `4`             | Maximum typed-IR transcription batches running concurrently |
| `GROKRXIV_FORMALIZE_TYPED_IR_INCLUDE_CONTEXT` | `0`             | Include definition/remark/context inventory entries in typed-IR; default transcribes only theorem-level Lean targets |
| `GROKRXIV_FORMALIZE_TYPED_IR_MAX_ITEMS` | `8`                 | Maximum selected theorem-level inventory items for the default typed-IR pass; set `0` for full selected inventory |
| `GROKRXIV_FORMALIZE_TYPED_IR_ONLY` | `0`                         | Benchmark mode: run typed-IR artifact refresh through `agh app run grokrxiv formalize <review_id>` and then stop before semantic/Lean stages |
| `GROKRXIV_FORMALIZE_QUEUE_AUTOSTART` | `1`                    | Start a one-shot `agh app work --run-id <job>` worker after queueing Lean formalization; set `0`/`false`/`no`/`off` to require manual workers |
| `GROKRXIV_LEAN_TARGET_CONCURRENCY` | `3`                        | Maximum concurrent per-theorem Lean author/check/fix jobs |
| `GROKRXIV_LEAN_MAX_TARGETS`      | `8`                         | MVP default Lean target cap; set `0` for full theorem-level mode, or another positive value for an explicit budget |
| `AGENTHERO_AGH_BIN`              | `agh`                       | Binary used by GrokRxiv when autostarting a one-shot app-run worker |
| `AGENTHERO_RUNTIME_ROOT`         | repo-root `.agenthero`       | AgentHero runtime state root; app-run logs are written under `$AGENTHERO_RUNTIME_ROOT/app_runs/` |
| `AGENTHERO_LOG_FILE`             | _none_                      | Optional structured AgentHero process log destination; writes JSONL diagnostics for audit pipelines |
| `AGENTHERO_SCHEDULER_WORKERS`    | `1`                         | Number of local app-run scheduler workers started by `agh serve` |
| `AGENTHERO_MODERATOR`           | _none_                      | Moderator handle persisted on `moderation_queue` rows |
| `GROKRXIV_PANDOC_BIN`          | `pandoc`                    | TeX-to-Markdown converter binary. Docker images install official Pandoc by default; local installs use PATH unless overridden |
| `GROKRXIV_DOCKER_INSTALL_PANDOC` | `1`                       | docker-compose build arg. Set `0` before build to omit Pandoc from the orchestrator image |
| `GROKRXIV_DOCKER_INSTALL_AGENT_CLIS` | `1`                    | docker-compose build arg. Installs Claude, Codex, and Antigravity `agy`; `agy` is installed with `curl -fsSL https://antigravity.google/cli/install.sh \| bash -s -- --dir /usr/local/bin` |
| `GROKRXIV_ORCHESTRATOR_PLATFORM` | `linux/arm64`             | Local Docker platform for orchestrator; set `linux/amd64` only when ARM is unavailable |
| `GROKRXIV_TEX_ENABLE_LATEXML`  | _none_                      | Opt into LaTeXML semantic AST enrichment. Pandoc remains the default TeX-to-Markdown converter |
| `GROKRXIV_TEX_DISABLE_LATEXML` | _none_                      | Force LaTeXML enrichment off even if `GROKRXIV_TEX_ENABLE_LATEXML=1` is present |
| `GROKRXIV_LATEXML_BIN`         | `latexml`                   | Optional LaTeXML binary checked only when `GROKRXIV_TEX_ENABLE_LATEXML=1` |
| `GROKRXIV_LATEXMLPOST_BIN`     | `latexmlpost`               | Optional LaTeXML postprocessor checked only when `GROKRXIV_TEX_ENABLE_LATEXML=1` |
| `GROKRXIV_EXTRACTION_MODE`     | `pandoc_enabled`            | Extraction execution mode: `pandoc_enabled` runs Pandoc/PDF/Rust tool extraction; `agent_enabled` runs extraction LLM tool loops with local tool fallbacks |

## Provider keys — API runner

| Env                                | Required by | Reachable check (`doctor`) |
|------------------------------------|-------------|---------------------------|
| `ANTHROPIC_API_KEY`                | Claude      | key-present only          |
| `OPENAI_API_KEY`                   | OpenAI      | GET `/v1/models`          |
| `GOOGLE_GENERATIVE_AI_API_KEY`     | Gemini      | key-present only          |
| `VLLM_BASE_URL`                    | vLLM        | key-present only          |

## CLI runner (local subprocess)

| Env                          | Notes |
|------------------------------|-------|
| `CLAUDE_CONFIG_DIR`          | Where the local `claude` CLI looks for auth (`~/.claude` typical) |
| `CODEX_HOME`                 | Where the local `codex` CLI looks for auth (`~/.codex` typical) |
| `AGENTHERO_ANTIGRAVITY_BIN`  | Optional override for the local Antigravity CLI binary used by `provider: gemini` roles. Defaults to `agy` |
| `AGENTHERO_AGY_BIN`          | Short alias for `AGENTHERO_ANTIGRAVITY_BIN` |
| `AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER` | Disabled unless set. Use `gemini` to fall back to Antigravity/agy only after a structured CLI quota error |
| `AGENTHERO_CLI_QUOTA_FALLBACK_MODEL` | Fallback model for the selected provider; defaults to `Gemini 3.5 Flash (Medium)` for agy/Gemini |
| `AGENTHERO_CLI_TIMEOUT_SECS`  | Global per-call timeout in the CLI runner. Role-specific `GROKRXIV_<ROLE>_TIMEOUT_SECS` vars take precedence |
| `GROKRXIV_CITATION_REVIEW_DETERMINISTIC` | Set `1` only to force the old deterministic no-LLM citation review fallback |
| `AGENTHERO_EXTRACTION_TOOL_FALLBACK` | Legacy `api` escape hatch for old scripts; refused unless direct provider API is explicitly allowed |

## Citation API resolver

These control deterministic citation existence checks. They are separate from
the Sonnet citation critic, which reviews citation relevance and missing prior
work after resolver evidence exists.

| Env | Default | Notes |
|-----|---------|-------|
| `GROKRXIV_CITATION_VERIFY_BUDGET_SECS` | `600` | Whole-paper citation API validation budget |
| `GROKRXIV_CITATION_ENTRY_CONCURRENCY` | `16` | Concurrent citation entries checked while preserving per-host throttles |
| `GROKRXIV_CITATION_REQUEST_TIMEOUT_SECS` | `15` | Per-request timeout for Crossref, arXiv, DOI, and generic JSON/text calls |
| `GROKRXIV_CITATION_PROVIDER_TIMEOUT_SECS` | `10` | Per-request timeout for fallback bibliographic providers |

When the resolved runtime is `--runner cli --extractor cli`, GrokRxiv removes
provider API key env vars from child `claude` / `codex` / `agy` processes so
those CLIs use their own logged-in local auth instead of inherited API keys.

For Docker on macOS, export Claude Code's Keychain-backed OAuth item into a
restricted file before starting compose:

```sh
security find-generic-password -s 'Claude Code-credentials' -w \
  > ~/.claude/docker-claude-code-credentials.secret
chmod 600 ~/.claude/docker-claude-code-credentials.secret
```

The orchestrator entrypoint copies that file into Claude Code's Linux
credentials paths inside `/home/grokrxiv`. Codex uses `~/.codex/auth.json`.
Gemini-family local roles use Antigravity CLI (`agy`). Antigravity migrates
legacy Gemini CLI sessions into native keyring storage and writes non-secret
state under paths such as `~/.gemini/antigravity-cli` and
`~/.gemini/antigravity`.

## Publisher

| Env                          | Notes |
|------------------------------|-------|
| `GITHUB_TOKEN`               | PAT used by `agh app run grokrxiv approve`; required for live PR creation |
| `GROKRXIV_REVIEWS_OWNER`     | Default `GrokRxiv` |
| `GROKRXIV_REVIEWS_REPO`      | Backward-compatible public repo alias; default `grokrxiv-reviews` |
| `GROKRXIV_PUBLIC_REVIEWS_REPO` | Public review repo, e.g. `GrokRxiv/grokrxiv-reviews` |
| `GROKRXIV_PRIVATE_REVIEWS_REPO` | Optional paid-private archive repo, e.g. `GrokRxiv/grokrxiv-private-reviews` |

## Web tier (`agenthero/apps/grokrxiv/web`)

| Env                                  | Notes |
|--------------------------------------|-------|
| `NEXT_PUBLIC_SITE_URL`               | Used by `agh app run grokrxiv open` |
| `GROKRXIV_PUBLIC_URL`                | Canonical URL (defaults to `https://grokrxiv.org`) |
| `ORCHESTRATOR_INTERNAL_URL`          | Internal orchestrator URL (default `http://localhost:8080`) |
| `AGENTHERO_SERVICE_TOKEN`             | Operator token for private proxy routes, not public read API access |
| `NEXT_PUBLIC_SUPABASE_URL`           | Supabase URL for read endpoints |
| `NEXT_PUBLIC_SUPABASE_ANON_KEY`      | Supabase anon key |
| `SUPABASE_SERVICE_ROLE_KEY`          | Server-only key for privileged web routes |
| `REVALIDATE_SECRET`                  | Required on the revalidate route |
| `GROKRXIV_BILLING_ENABLED`           | Set to `1` only when Stripe checkout is configured |
| `STRIPE_SECRET_KEY`                  | Stripe server key for checkout, portal, and webhooks |
| `STRIPE_WEBHOOK_SECRET`              | Stripe webhook signing secret |
| `STRIPE_SUPPORTER_PRICE_ID`          | Stripe recurring price id for the Supporter plan |
| `STRIPE_RESEARCHER_PRICE_ID`         | Stripe recurring price id for the Researcher plan |
| `GROKRXIV_SUPER_ADMIN_EMAIL`         | Optional admin seed/default account |
| `GROKRXIV_FREE_REVIEW_LIMIT`         | Free review quota shown in the dashboard; default `3` |

## Supabase Auth SMTP

| Env                 | Notes |
|---------------------|-------|
| `SMTP_HOST`         | Mailgun SMTP host, `smtp.mailgun.org` for US domains |
| `SMTP_PORT`         | `587` |
| `SMTP_USER`         | Mailgun SMTP user, e.g. `postmaster@appmail.magnetonlabs.com` |
| `SMTP_PASS`         | Mailgun SMTP password; secret, never `NEXT_PUBLIC_*` |
| `SMTP_ADMIN_EMAIL`  | Sender address, e.g. `no-reply@appmail.magnetonlabs.com` |
| `SMTP_SENDER_NAME`  | Sender display name, e.g. `GrokRxiv` |

Supabase Auth sends magic-link email. The web app should not send login email
through EmailJS or any browser-side mail provider. Local Supabase keeps SMTP
enabled in `supabase/config.toml`, but routes it to the local Mailpit/Inbucket
container instead of Mailgun.
