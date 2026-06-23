#!/usr/bin/env bash
# GrokRxiv local-stack preflight.
#
# Verifies that the local stack, database, and configured review runners are
# reachable before running the slower smoke tests.
#
# Usage:   bash agenthero/apps/grokrxiv/scripts/preflight.sh

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_ROOT="$(cd "${HERE}/.." && pwd)"

ok()   { printf "\033[32m✓\033[0m %s\n" "$*"; }
step() { printf "\033[36m→\033[0m %s\n" "$*"; }
fail() { printf "\033[31m✗\033[0m %s\n" "$*"; exit 1; }

step "docker daemon"
docker info >/dev/null 2>&1 \
  || fail "Docker daemon not running. Run \`open -a Docker\` and retry."
ok "docker up"

step "supabase containers"
if ! docker ps --format '{{.Names}}' | grep -q "^supabase_db_grokrxiv$"; then
  step "supabase not running — starting (this can take ~30 s on cold cache)"
  command -v supabase >/dev/null 2>&1 \
    || fail "supabase CLI not on PATH. brew install supabase/tap/supabase"
  supabase start >/dev/null 2>&1 \
    || fail "supabase start failed — run it manually for the error message"
fi
ok "supabase up"

step "postgres reachable on 127.0.0.1:54322"
CID="$(docker ps -qf 'name=supabase_db_grokrxiv' | head -1)"
[[ -n "${CID}" ]] || fail "no supabase_db_grokrxiv container found"
docker exec -i "${CID}" pg_isready -U postgres -d postgres >/dev/null 2>&1 \
  || fail "postgres not accepting connections inside the container"
ok "postgres ready"

step "review runner auth"
if [[ "${AGENTHERO_RUNNER:-cli}" == "api" || "${AGENTHERO_EXTRACTOR:-cli}" == "api" ]]; then
  if [[ -z "${ANTHROPIC_API_KEY:-}${OPENAI_API_KEY:-}${GOOGLE_GENERATIVE_AI_API_KEY:-}" ]]; then
    fail "API runner/extractor selected but no provider API key is set."
  fi
  ok "provider API key present for API path"
else
  command -v claude >/dev/null 2>&1 || fail "claude CLI not on PATH"
  command -v codex  >/dev/null 2>&1 || fail "codex CLI not on PATH"
  command -v agy >/dev/null 2>&1 || fail "Antigravity agy CLI not on PATH"
  ok "CLI runners present"
fi

step "agent routing lint"
bash "${APP_ROOT}/scripts/validate-agent-routing.sh"
ok "agent routing lint passed"

step "compose services (informational — not required to be up at this point)"
if curl -sf -m 2 http://localhost:8080/healthz >/dev/null 2>&1; then
  ok "orchestrator healthy"
else
  step "orchestrator not yet up (run \`docker compose -f agenthero/apps/grokrxiv/infra/docker-compose.yml up -d\` next)"
fi
if curl -sf -m 2 http://localhost:3000/ >/dev/null 2>&1; then
  ok "web up"
else
  step "web not yet up (run \`docker compose -f agenthero/apps/grokrxiv/infra/docker-compose.yml up -d\` next)"
fi

echo
ok "preflight PASSED"
