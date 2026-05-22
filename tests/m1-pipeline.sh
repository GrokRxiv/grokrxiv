#!/usr/bin/env bash
# M1 smoke — collapsed to AgentHero CLI calls per RPT2 Track I.
#
# Replaces the older psql-driven multi-step assertion script. The single
# `agh grokrxiv review ... --json` invocation runs the full ingest+DAG+verify
# loop; the JSON envelope carries the same evidence the SQL assertions
# previously asserted on (six agents, all pass, status awaiting_moderation).
#
# Requires:  agh (`just install`), jq, an active DATABASE_URL, and the
#            configured runner/extractor backend. Defaults to CLI/subscription
#            mode and explicitly disables direct provider API fallback.
#
# Usage:     tests/m1-pipeline.sh [arxiv_id]      (default 2605.12484)
#
# Env knobs:
#   AGENTHERO_RUNNER=cli|api       default cli
#   AGENTHERO_EXTRACTOR=cli|api    default cli
#   SKIP_APPROVE=1                stop after the review JSON assertion
#   AGENTHERO_BIN                path to the binary (defaults to `agh`)

set -euo pipefail

# The agh binary loads .env itself. Loading it here keeps shell-side
# checks and approve/publish smoke behavior aligned with the binary.
if [[ -f ".env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source ".env"
  set +a
fi

ARXIV_ID="${1:-2605.12484}"
BIN="${AGENTHERO_BIN:-agh}"
RUNNER="${AGENTHERO_RUNNER:-cli}"
EXTRACTOR="${AGENTHERO_EXTRACTOR:-cli}"
read -r -a AGENTHERO_CMD <<< "${BIN}"

if ! command -v "${AGENTHERO_CMD[0]}" >/dev/null 2>&1; then
  echo "fatal: ${AGENTHERO_CMD[0]} not on PATH — run \`just install\` (or set AGENTHERO_BIN)" >&2
  exit 2
fi

if [[ "${RUNNER}" == "api" || "${EXTRACTOR}" == "api" ]]; then
  export AGENTHERO_ALLOW_PROVIDER_API=1
  "${AGENTHERO_CMD[@]}" --runner "${RUNNER}" --extractor "${EXTRACTOR}" doctor --json \
    | jq -e '(.api_runners.anthropic.status == "ok")
          or (.api_runners.openai.status    == "ok")
          or (.api_runners.gemini.status    == "ok")' >/dev/null \
    || { echo "fatal: no usable API runner; see \`${BIN} doctor\`" >&2; exit 2; }
else
  export AGENTHERO_ALLOW_PROVIDER_API=0
  unset ANTHROPIC_API_KEY OPENAI_API_KEY GOOGLE_GENERATIVE_AI_API_KEY GOOGLE_API_KEY GEMINI_API_KEY
  for cli in claude codex gemini; do
    command -v "${cli}" >/dev/null 2>&1 \
      || { echo "fatal: ${cli} CLI not on PATH for CLI runner smoke" >&2; exit 2; }
  done
  "${AGENTHERO_CMD[@]}" --runner "${RUNNER}" --extractor "${EXTRACTOR}" --no-cache config --json \
    | jq -e '.runtime.direct_provider_api_allowed == false' >/dev/null \
    || { echo "fatal: direct provider API fallback is enabled for CLI smoke" >&2; exit 2; }
fi

# Ingest + review.
out="$("${AGENTHERO_CMD[@]}" --runner "${RUNNER}" --extractor "${EXTRACTOR}" --status --no-cache --json grokrxiv review "$ARXIV_ID")"

echo "$out" | jq -e '
    .review_id
    and (.status == "awaiting_moderation")
    and ((.agents | length) == 6)
    and (all(.agents[]; .verifier_status == "pass"))
' >/dev/null || { echo "$out" | jq .; echo "fatal: review JSON did not match invariants" >&2; exit 1; }

rid="$(echo "$out" | jq -r .review_id)"

if [[ "${SKIP_APPROVE:-0}" == "1" ]]; then
  echo "M1 (no-publish) PASS  review_id=$rid"
  exit 0
fi

# Approve + assert PR URL.
appr="$("${AGENTHERO_CMD[@]}" --json grokrxiv approve "$rid")"
echo "$appr" | jq -e '.pr_url | test("^https://github.com/")' >/dev/null \
  || { echo "$appr" | jq .; echo "fatal: approve did not return a github.com PR URL" >&2; exit 1; }

echo "M1 PASS  review_id=$rid"
