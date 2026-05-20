#!/usr/bin/env bash
# GrokRxiv publish-path E2E (opt-in).
#
# By default this exercises the product path through PR handoff only:
# ingest -> review -> approve -> PR opened at `pr_open`. It does NOT merge.
# Set GROKRXIV_E2E_ALLOW_MERGE=1 only for disposable test repositories when
# validating the destructive merge -> webhook -> published leg.
#
# Required env:
#   GITHUB_TOKEN                 — fine-grained PAT with content+pr write
#   GROKRXIV_REVIEWS_OWNER       — default GrokRxiv
#   GROKRXIV_REVIEWS_REPO        — default grokrxiv-reviews
#   GITHUB_WEBHOOK_SECRET        — must match the orchestrator's secret
#   DATABASE_URL                 — supabase Postgres
#
# Optional:
#   ARXIV_ID                     — default 2605.12484
#   GROKRXIV_RUNNER              — default cli
#   GROKRXIV_EXTRACTOR           — default cli
#   GROKRXIV_BIN                 — default: cargo run --quiet --release -p grokrxiv-orchestrator --
#   GROKRXIV_E2E_ALLOW_MERGE     — default 0; set 1 to merge the PR and publish
#
# Exits non-zero on any failed assertion.

set -euo pipefail

ARXIV_ID="${ARXIV_ID:-2605.12484}"
GROKRXIV_RUNNER="${GROKRXIV_RUNNER:-cli}"
GROKRXIV_EXTRACTOR="${GROKRXIV_EXTRACTOR:-cli}"
GROKRXIV_E2E_ALLOW_MERGE="${GROKRXIV_E2E_ALLOW_MERGE:-0}"
GROKRXIV_REVIEWS_OWNER="${GROKRXIV_REVIEWS_OWNER:-GrokRxiv}"
GROKRXIV_REVIEWS_REPO="${GROKRXIV_REVIEWS_REPO:-grokrxiv-reviews}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/.." && pwd)"
cd "${REPO}"

load_dotenv() {
  local env_file="${REPO}/.env"
  [[ -f "${env_file}" ]] || return 0
  set -a
  # shellcheck disable=SC1090
  source "${env_file}"
  set +a
}

load_dotenv

step() { printf "\n\033[36m▸ %s\033[0m\n" "$*"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { printf "  \033[31m✗\033[0m %s\n" "$*"; exit 1; }

: "${GITHUB_TOKEN:?GITHUB_TOKEN required for publish E2E}"
: "${GITHUB_WEBHOOK_SECRET:?GITHUB_WEBHOOK_SECRET required (must match orchestrator env)}"
: "${DATABASE_URL:?DATABASE_URL required (e.g. postgres://postgres:postgres@127.0.0.1:54322/postgres)}"

if [[ "${GROKRXIV_RUNNER}" == "api" || "${GROKRXIV_EXTRACTOR}" == "api" ]]; then
  export GROKRXIV_ALLOW_PROVIDER_API=1
  if [[ -z "${ANTHROPIC_API_KEY:-}${OPENAI_API_KEY:-}${GOOGLE_GENERATIVE_AI_API_KEY:-}" ]]; then
    fail "GROKRXIV_RUNNER=${GROKRXIV_RUNNER} GROKRXIV_EXTRACTOR=${GROKRXIV_EXTRACTOR} requires at least one provider API key"
  fi
else
  export GROKRXIV_ALLOW_PROVIDER_API=0
  unset ANTHROPIC_API_KEY OPENAI_API_KEY GOOGLE_GENERATIVE_AI_API_KEY GOOGLE_API_KEY GEMINI_API_KEY
  unset GROKRXIV_EXTRACTION_TOOL_FALLBACK
fi

command -v gh   >/dev/null || fail "gh CLI not on PATH"
command -v jq   >/dev/null || fail "jq not on PATH"
command -v curl >/dev/null || fail "curl not on PATH"
if [[ "${GROKRXIV_RUNNER}" == "cli" || "${GROKRXIV_EXTRACTOR}" == "cli" ]]; then
  command -v claude >/dev/null || fail "claude CLI not on PATH"
  command -v codex  >/dev/null || fail "codex CLI not on PATH"
  command -v gemini >/dev/null || fail "gemini CLI not on PATH"
fi

if [[ -n "${GROKRXIV_BIN:-}" ]]; then
  grokrxiv_cmd=("${GROKRXIV_BIN}")
else
  grokrxiv_cmd=(cargo run --quiet --release -p grokrxiv-orchestrator --)
fi

extract_json_field() {
  local file="$1"
  local filter="$2"
  local json_line
  json_line="$(awk '/^[[:space:]]*[{[]/ { line=$0 } END { print line }' "${file}")"
  if [[ -n "${json_line}" ]]; then
    jq -er "${filter}" <<<"${json_line}" 2>/dev/null && return 0
  fi
  return 1
}

run_psql() {
  if command -v psql >/dev/null 2>&1; then
    PGPASSWORD=postgres psql "${DATABASE_URL}" "$@"
  else
    local cid
    cid="$(docker ps -qf 'name=supabase_db_grokrxiv' | head -1)"
    [[ -n "${cid}" ]] || fail "neither psql nor a supabase_db_grokrxiv container is available"
    docker exec -i "${cid}" psql -U postgres -d postgres "$@"
  fi
}

step "1. ingest ${ARXIV_ID} via runner=${GROKRXIV_RUNNER} extractor=${GROKRXIV_EXTRACTOR}"
"${grokrxiv_cmd[@]}" --runner "${GROKRXIV_RUNNER}" --extractor "${GROKRXIV_EXTRACTOR}" --status --no-cache --json ingest "${ARXIV_ID}" 2>&1 \
  | tee /tmp/grokrxiv-publish-ingest.log
review_id="$(extract_json_field /tmp/grokrxiv-publish-ingest.log 'if type == "array" then .[0].review_id else .review_id end' \
  || awk '/review_id=/{ for(i=1;i<=NF;i++){ if($i ~ /^review_id=/){ split($i,a,"="); print a[2] } } }' /tmp/grokrxiv-publish-ingest.log | tail -1 | tr -d '\r')"
[[ -n "${review_id}" ]] || fail "no review_id from ingest"
ok "review_id=${review_id}"

step "2. approve → real PR on ${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}"
"${grokrxiv_cmd[@]}" --status --json approve "${review_id}" 2>&1 \
  | tee /tmp/grokrxiv-publish-approve.log
pr_url="$(extract_json_field /tmp/grokrxiv-publish-approve.log '.pr_url' \
  || awk '/pr_url=/{ for(i=1;i<=NF;i++){ if($i ~ /^pr_url=/){ split($i,a,"="); print a[2] } } }' /tmp/grokrxiv-publish-approve.log | tail -1 | tr -d '\r')"
[[ "${pr_url}" =~ ^https://github.com/.+/pull/[0-9]+$ ]] \
  || fail "expected real PR URL, got '${pr_url}'"
ok "PR opened: ${pr_url}"

step "3. assert PR handoff stopped before publish"
row="$(run_psql -tA -c "select status, published_at is null from reviews where id = '${review_id}'")"
[[ "${row}" == "pr_open|t" ]] || fail "expected 'pr_open|t', got '${row}'"
ok "review is pr_open and unpublished"

if [[ "${GROKRXIV_E2E_ALLOW_MERGE}" != "1" ]]; then
  ok "PR handoff E2E PASSED. Review ${pr_url}, then merge it manually to publish."
  echo
  ok "Set GROKRXIV_E2E_ALLOW_MERGE=1 only in a disposable repo to test merge webhook publication."
  exit 0
fi

step "4. merge the PR with gh (destructive opt-in enabled)"
pr_number="${pr_url##*/}"
gh pr merge --merge --delete-branch -R "${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}" "${pr_number}" \
  || fail "gh pr merge failed"
merge_sha="$(gh pr view "${pr_number}" -R "${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}" --json mergeCommit -q '.mergeCommit.oid')"
ok "merged at ${merge_sha}"

step "5. POST a signed pull_request.closed webhook to localhost:8080"
payload="$(jq -n \
  --arg arxiv_id "${ARXIV_ID}" \
  --arg review_id "${review_id}" \
  --arg sha "${merge_sha}" \
  --arg pr_url "${pr_url}" \
  '{
     action: "closed",
     pull_request: {
       merged: true,
       merge_commit_sha: $sha,
       html_url: $pr_url,
       title: "Review",
       head: { ref: ("review/" + $arxiv_id + "-" + ($sha[0:8])) },
       body: ("grokrxiv-review-id: " + $review_id)
     },
     repository: { full_name: env.GROKRXIV_REVIEWS_OWNER + "/" + env.GROKRXIV_REVIEWS_REPO }
   }')"
sig="sha256=$(printf '%s' "${payload}" | openssl dgst -sha256 -hmac "${GITHUB_WEBHOOK_SECRET}" | awk '{print $2}')"
http_status="$(curl -sS -o /tmp/grokrxiv-publish-webhook.json -w "%{http_code}" \
  -X POST http://localhost:8080/webhook/github \
  -H "Content-Type: application/json" \
  -H "X-Hub-Signature-256: ${sig}" \
  --data "${payload}")"
[[ "${http_status}" == "200" ]] || fail "webhook POST returned ${http_status}: $(cat /tmp/grokrxiv-publish-webhook.json)"
ok "webhook accepted (HTTP 200)"

step "6. assert reviews.status='published' + published_at set"
row="$(run_psql -tA -c "select status, published_at is not null from reviews where id = '${review_id}'")"
[[ "${row}" == "published|t" ]] || fail "expected 'published|t', got '${row}'"
ok "review row updated to published"

step "7. /api/v1/reviews/<id> returns the published review publicly"
WEB="${GROKRXIV_WEB_URL:-http://localhost:3000}"
public="$(curl -sf "${WEB}/api/v1/reviews/${review_id}" 2>/dev/null || true)"
[[ -n "${public}" ]] || fail "public API did not return the review"
echo "${public}" | jq -e '.status == "published"' >/dev/null \
  || fail "public API status is not 'published'"
ok "public API serves the review"

echo
ok "destructive publish E2E PASSED end-to-end."
