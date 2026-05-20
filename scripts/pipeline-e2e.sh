#!/usr/bin/env bash
# Full local frontend/backend acceptance chain for GrokRxiv.

set -euo pipefail

ARXIV_ID="${1:-2605.12484}"
BASE_URL="${GROKRXIV_BASE_URL:-http://localhost:3000}"
ORCH_URL="${ORCHESTRATOR_URL:-http://localhost:8080}"
DATABASE_URL="${DATABASE_URL:-postgresql://postgres:postgres@127.0.0.1:54322/postgres}"
export DATABASE_URL

step() { printf '\n\033[1;36m▸ %s\033[0m\n' "$*"; }
ok() { printf '  \033[1;32m✓\033[0m %s\n' "$*"; }
fail() { printf '  \033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

run_psql() {
  if command -v psql >/dev/null 2>&1; then
    PGPASSWORD=postgres psql "${DATABASE_URL}" "$@"
  else
    local cid
    cid="$(docker ps -qf 'name=supabase_db_grokrxiv' | head -1)"
    [[ -n "${cid}" ]] || fail "neither psql nor supabase_db_grokrxiv is available"
    docker exec -i "${cid}" psql -U postgres -d postgres "$@"
  fi
}

http_status() {
  local url="$1"
  curl -sS -o /dev/null -w '%{http_code}' "${url}" || true
}

step "preflight"
for cmd in curl jq unzip; do
  command -v "${cmd}" >/dev/null 2>&1 || fail "${cmd} is required"
done
bash scripts/preflight.sh
ok "preflight passed"

step "agent routing"
if [[ "${VALIDATE_PROVIDER_MODELS:-0}" == "1" ]]; then
  bash scripts/validate-agent-routing.sh --live
else
  bash scripts/validate-agent-routing.sh
fi
ok "agent routing lint passed"

step "compose health"
[[ "$(http_status "${ORCH_URL}/healthz")" == "200" ]] || fail "orchestrator is not healthy at ${ORCH_URL}"
[[ "$(http_status "${BASE_URL}/")" == "200" ]] || fail "web is not reachable at ${BASE_URL}"
ok "web and orchestrator are reachable"

step "preview happy path"
preview_body="$(mktemp)"
preview_status="$(curl -sS -o "${preview_body}" -w '%{http_code}' \
  -F "file=@tests/fixtures/sample.pdf;type=application/pdf" \
  "${ORCH_URL}/preview" || true)"
[[ "${preview_status}" == "200" ]] || fail "orchestrator /preview returned ${preview_status}: $(cat "${preview_body}")"
jq -e '.is_sample == true and (.html | contains("TL;DR")) and (.bundle_b64 | length > 100)' "${preview_body}" >/dev/null \
  || fail "preview response missing html/bundle sample fields"
ok "orchestrator preview returned sample html and zip bundle"

step "upload proxy preserves orchestrator validation status"
bad_pdf="$(mktemp)"
printf 'not a pdf' > "${bad_pdf}"
proxy_body="$(mktemp)"
proxy_status="$(curl -sS -o "${proxy_body}" -w '%{http_code}' \
  -F "file=@${bad_pdf};filename=bad.pdf;type=application/pdf" \
  "${BASE_URL}/api/upload" || true)"
[[ "${proxy_status}" == "415" ]] || fail "expected /api/upload 415 for non-PDF bytes, got ${proxy_status}: $(cat "${proxy_body}")"
jq -e '.upstream_status == 415 and (.hint | type == "string")' "${proxy_body}" >/dev/null \
  || fail "upload proxy response did not include upstream_status=415 and hint"
ok "upload proxy preserved 415 + hint"

step "public API seed review"
seed_status="$(http_status "${BASE_URL}/api/v1/reviews/22222222-2222-2222-2222-222222222222")"
[[ "${seed_status}" == "200" || "${seed_status}" == "503" ]] \
  || fail "seed review API expected 200 or Supabase 503, got ${seed_status}"
ok "public API reachable (status=${seed_status})"

step "CLI ingest/review pipeline"
pipeline_log="$(mktemp)"
SKIP_APPROVE=1 bash tests/m1-pipeline.sh "${ARXIV_ID}" | tee "${pipeline_log}"
review_id="$(awk '/review_id=/{ for(i=1;i<=NF;i++){ if($i ~ /^review_id=/){ split($i,a,"="); print a[2] } } }' "${pipeline_log}" | tail -1 | tr -d '\r')"
[[ -n "${review_id}" ]] || fail "could not recover review_id from ${pipeline_log}"
ok "review_id=${review_id}"

step "artifact bundle contains agent provenance"
for name in summary technical_correctness novelty reproducibility citation meta_reviewer; do
  unzip -l "artifacts/${review_id}/bundle.zip" "agents/${name}.json" >/dev/null \
    || fail "bundle.zip missing agents/${name}.json"
done
ok "bundle.zip contains all six agent JSON artifacts"

step "private review is not public before moderation"
private_status="$(http_status "${BASE_URL}/api/v1/reviews/${review_id}")"
[[ "${private_status}" == "404" || "${private_status}" == "503" ]] \
  || fail "awaiting_moderation review leaked through public API with status ${private_status}"
ok "awaiting_moderation review is not public (status=${private_status})"

if [[ "${RUN_PUBLISH_E2E:-0}" == "1" ]]; then
  step "PR handoff E2E"
  bash scripts/publish-e2e.sh "${ARXIV_ID}"
  ok "PR handoff E2E passed"
else
  step "PR handoff E2E skipped"
  ok "set RUN_PUBLISH_E2E=1 with GitHub env to run CLI PR handoff"
fi

printf '\n'
ok "pipeline E2E passed"
