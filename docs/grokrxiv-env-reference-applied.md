# `grokrxiv` env-var reference — applied

Env vars consumed by the orchestrator binary (`grokrxiv` / `grokrxiv-orchestrator`)
and the Next.js web tier. Layered config order is: CLI flags > ENV > TOML
profile > built-in defaults. The CLI's `--profile <name>` and `--config <path>`
flags pick the TOML file/profile that ENV then overrides.

## Service

| Env                            | Default                                      | Notes |
|--------------------------------|----------------------------------------------|-------|
| `ORCHESTRATOR_BIND`            | `0.0.0.0:8080`                              | axum bind address |
| `DATABASE_URL`                 | _unset_                                      | Required for persistence; otherwise stateless |
| `ARXIV_USER_AGENT`             | `GrokRxiv/0.1 (mailto:mlong168@gmail.com)`  | User-Agent string for arXiv |
| `PREVIEW_MODEL`                | `claude-opus-4-7`                            | Default Claude model id |
| `ADMIN_TOKEN`                  | _unset_                                      | Bearer for `/ingest` admin endpoint |
| `GITHUB_WEBHOOK_SECRET`        | _unset_                                      | HMAC secret for `/webhook/github` |
| `WEB_REVALIDATE_URL`           | _unset_                                      | Frontend revalidate endpoint |
| `REVALIDATE_SECRET`            | _unset_                                      | Bearer for the revalidate endpoint |

## Layered runtime (Track I)

| Env                            | CLI equivalent              | Notes |
|--------------------------------|-----------------------------|-------|
| `GROKRXIV_RUNNER`              | `--runner`                  | `api` / `cli` / `cloud` / `local_inference` |
| `GROKRXIV_SANDBOX`             | `--sandbox`                 | `none` / `container` |
| `GROKRXIV_MODE`                | `--mode`                    | `review_only` / `review_and_revise` |
| `GROKRXIV_CLOUD_PROVIDER`      | `--cloud-provider`          | `vercel_open_agents` / `e2b` / ... |
| `GROKRXIV_LITELLM_URL`         | `--litellm-url`             | LiteLLM gateway base URL |
| `OLLAMA_HOST`                  | `--ollama-host`             | Ollama direct base URL |
| `GROKRXIV_MAX_COST_USD`        | `--max-cost-usd`            | Hard ceiling per review |
| `GROKRXIV_NO_CACHE`            | `--no-cache`                | `1`/`true` to enable |
| `GROKRXIV_OFFLINE`             | `--offline`                 | `1`/`true` to enable |
| `GROKRXIV_SERVICE_TOKEN`       | _none_                      | Bearer expected by the web API `/api/v1/*` write endpoints |
| `GROKRXIV_AGENTS_DIR`          | _none_                      | Override `./agents` location |
| `GROKRXIV_MODERATOR`           | _none_                      | Moderator handle persisted on `moderation_queue` rows |

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
| `GROKRXIV_CLI_TIMEOUT_SECS`  | Per-call timeout in the CLI runner |

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
| `GITHUB_TOKEN`               | PAT used by `grokrxiv approve`. Absent → simulated PR |
| `GROKRXIV_REVIEWS_OWNER`     | Default `GrokRxiv` |
| `GROKRXIV_REVIEWS_REPO`      | Default `reviews` |

## Web tier (`apps/web`)

| Env                                  | Notes |
|--------------------------------------|-------|
| `NEXT_PUBLIC_SITE_URL`               | Used by `grokrxiv open` |
| `GROKRXIV_PUBLIC_URL`                | Canonical URL (defaults to `https://grokrxiv.org`) |
| `ORCHESTRATOR_INTERNAL_URL`          | The web API proxies `/api/v1/*` here (default `http://localhost:8080`) |
| `GROKRXIV_SERVICE_TOKEN`             | Bearer required on every `/api/v1/*` write endpoint |
| `NEXT_PUBLIC_SUPABASE_URL`           | Supabase URL for read endpoints |
| `NEXT_PUBLIC_SUPABASE_ANON_KEY`      | Supabase anon key |
| `REVALIDATE_SECRET`                  | Required on the revalidate route |
