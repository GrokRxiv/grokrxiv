# `grokrxiv` env-var reference — applied

Env vars consumed by the orchestrator binary (`grokrxiv` / `grokrxiv-orchestrator`)
and the Next.js web tier. Layered config order is: CLI flags > process ENV /
root `.env` / included `.env_*` files > TOML profile > built-in defaults. The
CLI's `--profile <name>` and `--config <path>` flags pick the TOML file/profile
that ENV then overrides.

## Env Files

The root `.env` is now a selector. It should normally contain only
`GROKRXIV_ENV_FILES`, for example:

```sh
GROKRXIV_ENV_FILES=.env_core,.env_ingest,.env_extract,.env_review,.env_publish,.env_web,.env_billing,.env_dev
```

The Rust CLI/orchestrator loads `.env` first, then loads the files named in
`GROKRXIV_ENV_FILES` relative to the root `.env` directory. Existing process
vars and root `.env` values win over included files.

Docker Compose does not follow `GROKRXIV_ENV_FILES` during `${...}`
interpolation. Before `docker compose up`, export the split env files into the
shell:

```sh
set -a
source .env
for file in ${GROKRXIV_ENV_FILES//,/ }; do source "$file"; done
set +a
```

| File | Purpose |
|------|---------|
| `.env_core` | Supabase/Postgres, orchestrator bind URLs, service/admin tokens, public base URLs |
| `.env_ingest` | arXiv/Crossref endpoints, data repo paths, storage, ingest scheduler and cache controls |
| `.env_extract` | Pandoc, LaTeXML, extraction mode, and extraction-agent toggles |
| `.env_review` | LLM provider keys, runners, role models, timeouts, review/verifier controls |
| `.env_publish` | GitHub publisher, webhook/revalidate secrets, publish-path E2E controls |
| `.env_web` | Next.js public env, web Supabase URLs, admin seed, SMTP |
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

## Layered runtime (Track I)

| Env                            | CLI equivalent              | Notes |
|--------------------------------|-----------------------------|-------|
| `GROKRXIV_RUNNER`              | `--runner`                  | `api` / `cli` / `cloud` / `local_inference` |
| `GROKRXIV_EXTRACTOR`           | `--extractor`               | Staged ingest extraction backend: `cli` / `api`; default `cli` |
| `GROKRXIV_SANDBOX`             | `--sandbox`                 | `none` / `container` |
| `GROKRXIV_MODE`                | `--mode`                    | `review_only` / `review_and_revise` |
| `GROKRXIV_CLOUD_PROVIDER`      | `--cloud-provider`          | `vercel_open_agents` / `e2b` / ... |
| `GROKRXIV_LITELLM_URL`         | `--litellm-url`             | LiteLLM gateway base URL |
| `OLLAMA_HOST`                  | `--ollama-host`             | Ollama direct base URL |
| `GROKRXIV_MAX_COST_USD`        | `--max-cost-usd`            | Hard ceiling per review |
| `GROKRXIV_FREE_REVIEW_LIMIT`   | _none_                      | Lifetime free full-review cap per logged-in user; default `3` |
| `GROKRXIV_NO_CACHE`            | `--no-cache`                | `1`/`true` to enable |
| `GROKRXIV_OFFLINE`             | `--offline`                 | `1`/`true` to enable |
| `GROKRXIV_ALLOW_PROVIDER_API`  | _internal_                  | Set by `grokrxiv`: `1` only when `--runner api`, `--extractor api`, or a per-role API override is selected |
| `GROKRXIV_SERVICE_TOKEN`       | _none_                      | Operator token for non-public web proxy routes; public `/api/v1` is read-only |
| `GROKRXIV_AGENTS_DIR`          | _none_                      | Override `./agents` location |
| `GROKRXIV_SUMMARY_MODEL`       | `claude-haiku-4-5-20251001` | Plain-language summary model; same role as `--model-for summary=...` |
| `GROKRXIV_TECHNICAL_CORRECTNESS_MODEL` | `claude-opus-4-7`      | Technical correctness model; same role as `--model-for technical_correctness=...` |
| `GROKRXIV_NOVELTY_MODEL`       | `gemini-3-flash-preview`    | Novelty model; same role as `--model-for novelty=...` |
| `GROKRXIV_REPRODUCIBILITY_MODEL` | `gpt-5.5`                  | Reproducibility model; same role as `--model-for reproducibility=...` |
| `GROKRXIV_CITATION_MODEL`      | `gemini-3-flash-preview`    | Citation model; same role as `--model-for citation=...` |
| `GROKRXIV_META_REVIEWER_MODEL` | `claude-sonnet-4-6`         | Meta-review synthesis model; same role as `--model-for meta_reviewer=...` |
| `GROKRXIV_SUMMARY_TIMEOUT_SECS` | `90`                       | CLI subprocess timeout for the summary role |
| `GROKRXIV_TECHNICAL_CORRECTNESS_TIMEOUT_SECS` | `600`           | CLI subprocess timeout for the technical correctness role |
| `GROKRXIV_NOVELTY_TIMEOUT_SECS` | `360`                      | CLI subprocess timeout for the novelty role |
| `GROKRXIV_REPRODUCIBILITY_TIMEOUT_SECS` | `240`              | CLI subprocess timeout for the reproducibility role |
| `GROKRXIV_CITATION_TIMEOUT_SECS` | `360`                     | CLI subprocess timeout for the citation role |
| `GROKRXIV_META_REVIEWER_TIMEOUT_SECS` | `300`               | CLI subprocess timeout for the meta reviewer role |
| `GROKRXIV_CITATION_PROMPT_MAX_BIB_ENTRIES` | `32`             | Maximum bibliography entries included in the Citation LLM relevance prompt; full bibliography still stays in artifacts/verifier data |
| `GROKRXIV_MODERATOR`           | _none_                      | Moderator handle persisted on `moderation_queue` rows |
| `GROKRXIV_PANDOC_BIN`          | `pandoc`                    | TeX-to-Markdown converter binary. Docker images install official Pandoc by default; local installs use PATH unless overridden |
| `GROKRXIV_DOCKER_INSTALL_PANDOC` | `1`                       | docker-compose build arg. Set `0` before build to omit Pandoc from the orchestrator image |
| `GROKRXIV_DOCKER_INSTALL_AGENT_CLIS` | `1`                    | docker-compose build arg. Installs Claude, Codex, and Gemini CLIs into the orchestrator image |
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
| `GEMINI_HOME`                | Where the local `gemini` CLI looks for auth |
| `GROKRXIV_CLI_TIMEOUT_SECS`  | Global per-call timeout in the CLI runner. Role-specific `GROKRXIV_<ROLE>_TIMEOUT_SECS` vars take precedence |
| `GROKRXIV_CITATION_REVIEW_DETERMINISTIC` | Set `1` only to force the old deterministic no-LLM citation review fallback |
| `GROKRXIV_EXTRACTION_TOOL_FALLBACK` | Legacy `api` escape hatch for old scripts; refused unless direct provider API is explicitly allowed |

When the resolved runtime is `--runner cli --extractor cli`, GrokRxiv removes
provider API key env vars from child `claude` / `codex` / `gemini` processes so
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
Gemini uses `~/.gemini/oauth_creds.json` plus
`~/.gemini/google_accounts.json`; the entrypoint writes a minimal container
`settings.json` selecting OAuth personal auth instead of copying host MCP/trust
settings.

## Cloud runner

| Env                          | Notes |
|------------------------------|-------|
| `VERCEL_OPEN_AGENTS_URL`     | Health-checked via `GET /healthz` by `doctor` |
| `VERCEL_OPEN_AGENTS_TOKEN`   | Bearer for Vercel Open Agents |
| `E2B_API_KEY`                | E2B sandbox key (presence-only check) |

## Local inference

| Env                          | Notes |
|------------------------------|-------|
| `GROKRXIV_LITELLM_URL`       | Preferred over `OLLAMA_HOST` |
| `LITELLM_URL`                | Alias accepted by `doctor` |
| `OLLAMA_HOST`                | e.g. `http://localhost:11434` |

## Publisher

| Env                          | Notes |
|------------------------------|-------|
| `GITHUB_TOKEN`               | PAT used by `grokrxiv approve`; required for live PR creation |
| `GROKRXIV_REVIEWS_OWNER`     | Default `GrokRxiv` |
| `GROKRXIV_REVIEWS_REPO`      | Backward-compatible public repo alias; default `grokrxiv-reviews` |
| `GROKRXIV_PUBLIC_REVIEWS_REPO` | Public review repo, e.g. `GrokRxiv/grokrxiv-reviews` |
| `GROKRXIV_PRIVATE_REVIEWS_REPO` | Optional paid-private archive repo, e.g. `GrokRxiv/grokrxiv-private-reviews` |

## Web tier (`apps/web`)

| Env                                  | Notes |
|--------------------------------------|-------|
| `NEXT_PUBLIC_SITE_URL`               | Used by `grokrxiv open` |
| `GROKRXIV_PUBLIC_URL`                | Canonical URL (defaults to `https://grokrxiv.org`) |
| `ORCHESTRATOR_INTERNAL_URL`          | Internal orchestrator URL (default `http://localhost:8080`) |
| `GROKRXIV_SERVICE_TOKEN`             | Operator token for private proxy routes, not public read API access |
| `NEXT_PUBLIC_SUPABASE_URL`           | Supabase URL for read endpoints |
| `NEXT_PUBLIC_SUPABASE_ANON_KEY`      | Supabase anon key |
| `REVALIDATE_SECRET`                  | Required on the revalidate route |
| `GROKRXIV_BILLING_ENABLED`           | Set to `1` only when Stripe checkout is configured |
| `STRIPE_SECRET_KEY`                  | Stripe server key for checkout, portal, and webhooks |
| `STRIPE_WEBHOOK_SECRET`              | Stripe webhook signing secret |
| `STRIPE_SUPPORTER_PRICE_ID`          | Stripe recurring price id for the Supporter plan |
| `STRIPE_RESEARCHER_PRICE_ID`         | Stripe recurring price id for the Researcher plan |

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
