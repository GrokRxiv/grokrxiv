# GrokRxiv — local-dev orchestration.
#
# `just`            → list recipes
# `just install`    → install JS + Rust deps
# `just supabase`   → start local Supabase + apply migrations
# `just dev`        → run web + orchestrator side-by-side
# `just web`        → run only Next.js dev server
# `just orch`       → run only the Rust orchestrator
# `just build`      → build everything (release mode)
# `just test`       → run all unit tests (workspace)
# `just test-pipeline arxiv_id=2605.12484` → drive one paper end-to-end
# `just test-landing`→ run Playwright browser test against localhost:3000
# `just clean`      → drop local Supabase + clean build artifacts

set shell := ["bash", "-uc"]
set dotenv-load := true

# Default M1 paper: arxiv:2605.12484, "Learning, Fast and Slow"
# (real May-2026 cs.LG paper, single primary category, manageable length).
arxiv_id := "2605.12484"

default:
    @just --list

install:
    pnpm install
    cargo install --path crates/orchestrator --features full --bin grokrxiv --locked
    @echo "✓ dependencies installed; `grokrxiv` is on $CARGO_HOME/bin"

# Install just the cargo dependencies without building the CLI binary.
install-deps:
    pnpm install
    cargo fetch
    @echo "✓ dependencies fetched"

supabase:
    @command -v supabase >/dev/null || {{ echo "supabase CLI not installed"; exit 1 }}
    bash infra/supabase/setup.sh

# Verify the local stack is healthy before running anything expensive.
# Checks docker, Supabase containers (auto-starts if absent), Postgres, and
# the ANTHROPIC_API_KEY env var. Run before `just dev` or `just smoke`.
preflight:
    bash scripts/preflight.sh

# Run the full local acceptance chain: preflight → frontend gates → cargo
# tests → docker compose up → Playwright → M1 pipeline. Single command,
# fail-fast.
smoke arxiv_id="2605.12484":
    bash scripts/preflight.sh
    cargo fmt --all -- --check
    cargo clippy --workspace --no-deps -- -D warnings
    RUSTFLAGS='-D warnings' cargo check --workspace --features full --locked
    pnpm --filter @grokrxiv/web typecheck
    pnpm --filter @grokrxiv/web lint
    cargo test --workspace
    docker compose up -d --build
    pnpm --filter @grokrxiv/e2e-web test
    DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:54322/postgres" bash scripts/pipeline-e2e.sh {{arxiv_id}}
    @echo "smoke PASS"

# Bring the full stack up with docker compose (postgres + migrate + orchestrator + web).
# Requires .env with at least ANTHROPIC_API_KEY=sk-ant-...
compose-up:
    docker compose up --build -d
    @echo "→ web:           http://localhost:3000"
    @echo "→ orchestrator:  http://localhost:8080"
    @echo "→ postgres:      postgres://postgres:postgres@localhost:54322/postgres"
    @echo "Tail logs with: docker compose logs -f"

compose-down:
    docker compose down

compose-logs service="":
    docker compose logs -f {{service}}

web:
    pnpm --filter @grokrxiv/web dev

orch:
    cargo run -p grokrxiv-orchestrator -- serve

# Run the orchestrator's HTTP API + supervisor + scheduler via the installed
# `grokrxiv` binary (after `just install`).
serve:
    grokrxiv serve

# Run the preflight checks via the installed `grokrxiv` binary.
doctor:
    grokrxiv doctor

# Install pandoc + latexml (TeX→Markdown + semantic-AST pipeline).
setup-pandoc:
    brew install pandoc latexml

dev:
    @echo "Starting web (3000) + orchestrator (8080) in parallel..."
    (trap 'kill 0' SIGINT; \
        cargo run -p grokrxiv-orchestrator -- serve & \
        pnpm --filter @grokrxiv/web dev & \
        wait)

build:
    cargo build --release --workspace
    pnpm --filter @grokrxiv/web build

# Workspace unit tests + frontend tests.
test:
    cargo test --workspace
    pnpm --filter @grokrxiv/web test

# Drive the full review DAG on one real arXiv paper. Requires Supabase + LLM
# API keys in .env. Used by milestone M1.
test-pipeline arxiv_id=arxiv_id:
    @echo "→ M1: ingest+review one paper: arxiv:{{arxiv_id}}"
    bash tests/m1-pipeline.sh "{{arxiv_id}}"

# Browser-level test of the landing page. Requires `pnpm dev` running first
# (use `just dev` in another shell).
test-landing:
    cd tests/e2e-web && pnpm install && pnpm exec playwright install --with-deps chromium && pnpm test

clean:
    -supabase stop 2>/dev/null
    cargo clean
    rm -rf apps/web/.next apps/web/node_modules node_modules tests/e2e-web/node_modules

# Bring up local Mac dev topology (Ollama on host; surrounding services in docker).
up-local:
    docker compose -f infra/compose.local.yml --env-file .env up -d
    @echo "✓ LiteLLM:    http://localhost:4000"
    @echo "✓ Open WebUI: http://localhost:3001"
    @echo "✓ Redis:      localhost:6379"
    @echo "→ Make sure Ollama is running natively: brew services start ollama"

# Bring up cloud topology (full containerized; needs nvidia-container-toolkit on host).
up-cloud:
    docker compose -f infra/compose.cloud.yml --env-file .env up -d
    @echo "✓ Ollama:       http://localhost:11434"
    @echo "✓ LiteLLM:      http://localhost:4000"
    @echo "✓ Orchestrator: http://localhost:8080"

# Tear down whichever stack is up (idempotent across both).
down:
    -docker compose -f infra/compose.local.yml down
    -docker compose -f infra/compose.cloud.yml down
