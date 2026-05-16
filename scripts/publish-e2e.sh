#!/usr/bin/env bash
# GrokRxiv publish-path E2E (opt-in).
#
# Exercises the FULL publication loop end-to-end against a disposable test
# repository on GitHub. Skipped from `just smoke` because it requires GitHub
# credentials and a real test repo; run manually when validating the publish
# flow.
#
# Required env:
#   GITHUB_TOKEN                 — fine-grained PAT with content+pr write
#   GROKRXIV_REVIEWS_OWNER       — e.g. your-username
#   GROKRXIV_REVIEWS_REPO        — a TEST repo (e.g. grokrxiv-reviews-test)
#   GITHUB_WEBHOOK_SECRET        — must match the orchestrator's secret
#   ANTHROPIC_API_KEY            — for the M1 ingest step
#   DATABASE_URL                 — supabase Postgres
#
# Optional:
#   ARXIV_ID                     — default 2605.12484
#
# Exits non-zero on any failed assertion.

set -euo pipefail

ARXIV_ID="${ARXIV_ID:-2605.12484}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/.." && pwd)"
cd "${REPO}"

step() { printf "\n\033[36m▸ %s\033[0m\n" "$*"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { printf "  \033[31m✗\033[0m %s\n" "$*"; exit 1; }

: "${GITHUB_TOKEN:?GITHUB_TOKEN required for publish E2E}"
: "${GROKRXIV_REVIEWS_OWNER:?GROKRXIV_REVIEWS_OWNER required (e.g. your-github-username)}"
: "${GROKRXIV_REVIEWS_REPO:?GROKRXIV_REVIEWS_REPO required (a disposable TEST repo)}"
: "${GITHUB_WEBHOOK_SECRET:?GITHUB_WEBHOOK_SECRET required (must match orchestrator env)}"
: "${ANTHROPIC_API_KEY:?ANTHROPIC_API_KEY required}"
: "${DATABASE_URL:?DATABASE_URL required (e.g. postgres://postgres:postgres@127.0.0.1:54322/postgres)}"

command -v gh   >/dev/null || fail "gh CLI not on PATH"
command -v jq   >/dev/null || fail "jq not on PATH"
command -v curl >/dev/null || fail "curl not on PATH"

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

step "1. M1 ingest ${ARXIV_ID}"
review_id="$(
  cargo run --quiet --release -p grokrxiv-orchestrator -- ingest "${ARXIV_ID}" 2>&1 \
    | tee /tmp/grokrxiv-publish-ingest.log \
    | awk '/review_id=/{ for(i=1;i<=NF;i++){ if($i ~ /^review_id=/){ split($i,a,"="); print a[2] } } }' | tr -d '\r'
)"
[[ -n "${review_id}" ]] || fail "no review_id from ingest"
ok "review_id=${review_id}"

step "2. approve → real PR on ${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}"
pr_url="$(
  cargo run --quiet --release -p grokrxiv-orchestrator -- approve "${review_id}" 2>&1 \
    | tee /tmp/grokrxiv-publish-approve.log \
    | awk '/pr_url=/{ for(i=1;i<=NF;i++){ if($i ~ /^pr_url=/){ split($i,a,"="); print a[2] } } }' | tr -d '\r'
)"
[[ "${pr_url}" =~ ^https://github.com/.+/pull/[0-9]+$ ]] \
  || fail "expected real PR URL, got '${pr_url}'"
ok "PR opened: ${pr_url}"

step "3. merge the PR with gh"
pr_number="${pr_url##*/}"
gh pr merge --merge --delete-branch -R "${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}" "${pr_number}" \
  || fail "gh pr merge failed"
merge_sha="$(gh pr view "${pr_number}" -R "${GROKRXIV_REVIEWS_OWNER}/${GROKRXIV_REVIEWS_REPO}" --json mergeCommit -q '.mergeCommit.oid')"
ok "merged at ${merge_sha}"

step "4. POST a signed pull_request.closed webhook to localhost:8080"
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

step "5. assert reviews.status='published' + published_at set"
row="$(run_psql -tA -c "select status, published_at is not null from reviews where id = '${review_id}'")"
[[ "${row}" == "published|t" ]] || fail "expected 'published|t', got '${row}'"
ok "review row updated to published"

step "6. /api/v1/reviews/<id> returns the published review publicly"
WEB="${GROKRXIV_WEB_URL:-http://localhost:3000}"
public="$(curl -sf "${WEB}/api/v1/reviews/${review_id}" 2>/dev/null || true)"
[[ -n "${public}" ]] || fail "public API did not return the review"
echo "${public}" | jq -e '.status == "published"' >/dev/null \
  || fail "public API status is not 'published'"
ok "public API serves the review"

echo
ok "publish E2E PASSED end-to-end."
