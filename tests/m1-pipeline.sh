#!/usr/bin/env bash
# M1 smoke — collapsed to grokrxiv CLI calls per RPT2 Track I.
#
# Replaces the older psql-driven multi-step assertion script. The single
# `grokrxiv review ... --json` invocation runs the full ingest+DAG+verify
# loop; the JSON envelope carries the same evidence the SQL assertions
# previously asserted on (six agents, all pass, status awaiting_moderation).
#
# Requires:  grokrxiv (`just install`), jq, an active DATABASE_URL + at least
#            one provider API key in the environment.
#
# Usage:     tests/m1-pipeline.sh [arxiv_id]      (default 2605.12484)
#
# Env knobs:
#   SKIP_APPROVE=1   stop after the review JSON assertion
#   GROKRXIV_BIN     path to the binary (defaults to `grokrxiv` on PATH)

set -euo pipefail

ARXIV_ID="${1:-2605.12484}"
BIN="${GROKRXIV_BIN:-grokrxiv}"

if ! command -v "$BIN" >/dev/null 2>&1; then
  echo "fatal: $BIN not on PATH — run \`just install\` (or set GROKRXIV_BIN)" >&2
  exit 2
fi

# Preflight: at least one API runner must be reachable.
"$BIN" doctor --json \
  | jq -e '(.api_runners.anthropic.status == "ok")
        or (.api_runners.openai.status    == "ok")
        or (.api_runners.gemini.status    == "ok")' >/dev/null \
  || { echo "fatal: no usable API runner; see \`$BIN doctor\`" >&2; exit 2; }

# Ingest + review.
out="$("$BIN" review "$ARXIV_ID" --json)"

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
appr="$("$BIN" approve "$rid" --json)"
echo "$appr" | jq -e '.pr_url | test("^https://github.com/")' >/dev/null \
  || { echo "$appr" | jq .; echo "fatal: approve did not return a github.com PR URL" >&2; exit 1; }

echo "M1 PASS  review_id=$rid"
