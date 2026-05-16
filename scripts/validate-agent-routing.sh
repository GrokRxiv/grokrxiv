#!/usr/bin/env bash
# Validate agents/*.yaml provider/model routing before an expensive DAG run.

set -euo pipefail

LIVE=0
if [[ "${1:-}" == "--live" ]]; then
  LIVE=1
fi

fail() { printf '✗ %s\n' "$*" >&2; exit 1; }
ok() { printf '✓ %s\n' "$*"; }
warn() { printf '→ %s\n' "$*" >&2; }

shopt -s nullglob
files=(agents/*.yaml)
[[ "${#files[@]}" -gt 0 ]] || fail "no agents/*.yaml files found"

for file in "${files[@]}"; do
  provider="$(awk '/^provider:/ {print $2; exit}' "${file}")"
  model="$(awk '/^model:/ {print $2; exit}' "${file}")"
  if [[ -z "${provider}" && -z "${model}" ]]; then
    warn "${file}: no provider/model block; treating as non-routing metadata"
    continue
  fi
  [[ -n "${provider}" ]] || fail "${file}: missing provider"
  [[ -n "${model}" ]] || fail "${file}: missing model"

  case "${provider}" in
    claude|openai|gemini|vllm) ;;
    *) fail "${file}: unknown provider '${provider}'" ;;
  esac

  case "${model}" in
    *TODO*|*todo*|*placeholder*|*example*|gpt-5-large)
      fail "${file}: model '${model}' is a placeholder or known-bad alias"
      ;;
  esac

  case "${provider}" in
    claude)
      [[ -n "${ANTHROPIC_API_KEY:-}" ]] || warn "${file}: claude key missing; DAG cannot use this route"
      ;;
    openai)
      if [[ -z "${OPENAI_API_KEY:-}" ]]; then
        warn "${file}: OPENAI_API_KEY missing; route will fall back to Claude"
      elif [[ "${LIVE}" == "1" ]]; then
        code="$(curl -sS -o /dev/null -w '%{http_code}' \
          -H "Authorization: Bearer ${OPENAI_API_KEY}" \
          "https://api.openai.com/v1/models/${model}" || true)"
        [[ "${code}" == "200" ]] || fail "${file}: OpenAI model '${model}' was not accepted by /v1/models (HTTP ${code})"

        body="$(mktemp)"
        if [[ "${model}" == gpt-5* || "${model}" == o* ]]; then
          payload="$(printf '{"model":"%s","messages":[{"role":"user","content":"Return OK."}],"max_completion_tokens":8,"reasoning_effort":"medium"}' "${model}")"
        else
          payload="$(printf '{"model":"%s","messages":[{"role":"user","content":"Return OK."}],"max_completion_tokens":8,"temperature":0}' "${model}")"
        fi
        code="$(curl -sS -o "${body}" -w '%{http_code}' \
          -H "Authorization: Bearer ${OPENAI_API_KEY}" \
          -H "Content-Type: application/json" \
          -d "${payload}" \
          "https://api.openai.com/v1/chat/completions" || true)"
        if [[ "${code}" != "200" ]]; then
          if grep -q '"code"[[:space:]]*:[[:space:]]*"insufficient_quota"' "${body}"; then
            fail "${file}: OpenAI quota exhausted for '${model}' (insufficient_quota)"
          fi
          fail "${file}: OpenAI chat completion smoke failed for '${model}' (HTTP ${code}): $(tr -d '\n' < "${body}")"
        fi
        rm -f "${body}"
      fi
      ;;
    gemini)
      [[ -n "${GOOGLE_GENERATIVE_AI_API_KEY:-}" ]] || warn "${file}: GOOGLE_GENERATIVE_AI_API_KEY missing; route will fall back to Claude"
      ;;
    vllm)
      [[ -n "${VLLM_BASE_URL:-}" ]] || warn "${file}: VLLM_BASE_URL missing; route will fall back to Claude"
      ;;
  esac
done

ok "agent routing validated (${#files[@]} files, live=${LIVE})"
