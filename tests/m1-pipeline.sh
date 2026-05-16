#!/usr/bin/env bash
# GrokRxiv M1 pipeline driver — one paper end-to-end.
#
# 1. ingest-one <arxiv_id>   → ingest + run full review DAG → awaiting_moderation
# 2. assert DB has 1 papers row, 6 review_agents rows, reviews.status='awaiting_moderation'
# 3. approve <review_id>     → orchestrator calls publisher::open_review_pr
# 4. assert reviews.status='pr_open' and github_pr_url is set
#
# Usage: tests/m1-pipeline.sh <arxiv_id>   (default 2605.12484)
#
# Requires: cargo, psql, jq, DATABASE_URL pointing at the running Supabase.
set -euo pipefail

ARXIV_ID="${1:-2605.12484}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/.." && pwd)"
cd "${REPO}"

: "${DATABASE_URL:?DATABASE_URL is required (run \`just supabase\` first)}"
: "${ANTHROPIC_API_KEY:?ANTHROPIC_API_KEY is required for the review DAG}"

step() { printf "\n\033[1;36m▸ %s\033[0m\n" "$*"; }
ok()   { printf "  \033[1;32m✓\033[0m %s\n" "$*"; }
fail() { printf "  \033[1;31m✗\033[0m %s\n" "$*"; exit 1; }

# Run a psql query against the local Supabase Postgres. Falls back to
# `docker exec` against the `supabase_db_*` container when the host doesn't
# have psql installed (common on macOS without `brew install libpq`).
run_psql() {
  if command -v psql >/dev/null 2>&1; then
    PGPASSWORD=postgres psql "${DATABASE_URL}" "$@"
  else
    local cid
    cid="$(docker ps -qf 'name=supabase_db_grokrxiv' | head -1)"
    if [[ -z "${cid}" ]]; then
      fail "neither psql nor a supabase_db_grokrxiv container is available"
    fi
    docker exec -i "${cid}" psql -U postgres -d postgres "$@"
  fi
}

step "1. cargo run -- ingest ${ARXIV_ID}"
review_id="$(
  cargo run --quiet --release -p grokrxiv-orchestrator -- \
    ingest "${ARXIV_ID}" \
  2>&1 | tee /tmp/grokrxiv-m1-ingest.log \
  | awk '/review_id=/{ for(i=1;i<=NF;i++){ if($i ~ /^review_id=/){ split($i,a,"="); print a[2] } } }' | tr -d '\r'
)"
[[ -n "${review_id}" ]] || fail "no review_id returned from ingest-one"
ok "review_id=${review_id}"

step "2. assert papers row exists"
run_psql -v ON_ERROR_STOP=1 -tA -c \
  "select count(*) from papers where arxiv_id = '${ARXIV_ID}'" \
  | grep -qx 1 || fail "papers row missing for ${ARXIV_ID}"
ok "papers row found"

step "3. assert 6 review_agents rows"
count="$(run_psql -tA -c \
  "select count(*) from review_agents where review_id = '${review_id}'")"
[[ "${count}" == "6" ]] || fail "expected 6 review_agents rows, got ${count}"
ok "6 review_agents rows"

step "4. assert reviews.status = 'awaiting_moderation'"
status="$(run_psql -tA -c \
  "select status from reviews where id = '${review_id}'")"
[[ "${status}" == "awaiting_moderation" ]] \
  || fail "expected awaiting_moderation, got '${status}'"
ok "status=awaiting_moderation"

step "5. assert real verifier ladder ran (citation + tone rungs present)"
cit="$(run_psql -tA -c \
  "select count(*) from review_agents where review_id = '${review_id}' \
   and verifier_notes ? 'citation'")"
tone="$(run_psql -tA -c \
  "select count(*) from review_agents where review_id = '${review_id}' \
   and verifier_notes ? 'tone'")"
[[ "${cit}" -gt 0 ]] || fail "no review_agent has a 'citation' rung in verifier_notes — ladder didn't run"
[[ "${tone}" -gt 0 ]] || fail "no review_agent has a 'tone' rung in verifier_notes — ladder didn't run"
ok "verifier ladder ran (citation rungs=${cit}, tone rungs=${tone})"

step "6. assert moderation_queue has a pending row"
mq=$(run_psql -tA -c "select state from moderation_queue where review_id='${review_id}'")
[[ "${mq}" == "pending" ]] || fail "expected moderation_queue.state=pending, got '${mq}'"
ok "moderation_queue pending"

step "7. assert render_to_disk wrote real artifacts"
for f in review.html review.md review.tex bundle.zip; do
  [[ -s "artifacts/${review_id}/${f}" ]] || fail "missing/empty artifacts/${review_id}/${f}"
done
title_needle="$(run_psql -tA -c "select title from papers where arxiv_id='${ARXIV_ID}'" | head -c 30)"
grep -q "${title_needle}" "artifacts/${review_id}/review.html" \
  || fail "review.html doesn't reference the real paper title"
ok "real artifacts on disk"

step "8. assert multi-provider routing (≥2 distinct models when keys for declared providers are present)"
models=$(run_psql -tA -c "select count(distinct model) from review_agents where review_id='${review_id}'")
# Count distinct providers declared across agents/*.yaml that ALSO have their
# API key set in the env. Only then is multi-provider routing actually
# exercisable; otherwise role_routing falls back to claude by design.
declared=$(awk -F': *' '/^provider: / {print $2}' agents/*.yaml | sort -u)
exercisable=0
for p in $declared; do
  case "$p" in
    claude)  [[ -n "${ANTHROPIC_API_KEY:-}" ]] && exercisable=$((exercisable+1)) ;;
    openai)  [[ -n "${OPENAI_API_KEY:-}" ]] && exercisable=$((exercisable+1)) ;;
    gemini)  [[ -n "${GOOGLE_GENERATIVE_AI_API_KEY:-}" ]] && exercisable=$((exercisable+1)) ;;
    vllm)    [[ -n "${VLLM_BASE_URL:-}" ]] && exercisable=$((exercisable+1)) ;;
  esac
done
if [[ "${exercisable}" -ge 2 ]]; then
  [[ "${models}" -ge 2 ]] || fail "expected ≥2 distinct models with ${exercisable} exercisable providers, got ${models}"
  ok "multi-provider routing (${models} distinct models, ${exercisable} exercisable providers)"
else
  ok "single-provider M1 ok (${exercisable} exercisable provider; others fell back to claude by design)"
fi

if [[ "${SKIP_APPROVE:-0}" == "1" ]]; then
  echo
  ok "M1 (no-publish) PASSED — 6 agents, awaiting_moderation, real verifier ladder evidence persisted."
  exit 0
fi

step "5. cargo run -- approve ${review_id}"
pr_url="$(
  cargo run --quiet --release -p grokrxiv-orchestrator --features full -- \
    approve "${review_id}" \
  | tee /tmp/grokrxiv-m1-approve.log \
  | awk '/pr_url=/{print $NF}' | tr -d '\r'
)"
[[ "${pr_url}" =~ ^https://github.com/GrokRxiv/reviews/pull/ ]] \
  || fail "expected GitHub PR URL, got '${pr_url}'"
ok "PR opened: ${pr_url}"

step "6. assert reviews.status = 'pr_open' + github_pr_url set"
row="$(run_psql -tA -c \
  "select status, github_pr_url from reviews where id = '${review_id}'")"
[[ "${row}" == "pr_open|${pr_url}" ]] || fail "row mismatch: ${row}"
ok "status=pr_open, github_pr_url=${pr_url}"

echo
ok "M1 PASSED end-to-end."
