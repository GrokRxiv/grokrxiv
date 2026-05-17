#!/usr/bin/env bash
# Manage the local GrokRxiv webhook + cloudflared tunnel.
#
# Usage:
#   scripts/dev-webhook.sh up       Start orchestrator (if needed), start
#                                   cloudflared, upsert the webhook on
#                                   GrokRxiv/grokrxiv-reviews. Idempotent.
#   scripts/dev-webhook.sh down     Kill cloudflared, delete the webhook.
#   scripts/dev-webhook.sh status   Show current orchestrator / tunnel / hook
#                                   state.
#   scripts/dev-webhook.sh logs     Tail cloudflared log.
#
# Prerequisites:
#   - cloudflared installed (`brew install cloudflared`)
#   - gh CLI authenticated against an account with write on
#     GrokRxiv/grokrxiv-reviews
#   - docker compose has the `orchestrator` service defined
#   - .env contains GITHUB_WEBHOOK_SECRET (auto-generated on first up).
#
# Notes:
#   - Uses a cloudflared "quick tunnel" so the public hostname changes on every
#     restart. Each `up` rewrites the webhook URL on the repo to match the new
#     tunnel. If you want a permanent hostname, switch to a named cloudflared
#     tunnel or ngrok with a reserved domain and edit this script.

set -euo pipefail

REPO="${GROKRXIV_REVIEWS_REPO:-GrokRxiv/grokrxiv-reviews}"
ORCH_PORT="${ORCHESTRATOR_PORT:-8080}"
ORCH_HEALTHZ="http://127.0.0.1:${ORCH_PORT}/healthz"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$ROOT/.env"
STATE_DIR="${TMPDIR:-/tmp}/grokrxiv-dev-webhook"
PID_FILE="$STATE_DIR/cloudflared.pid"
LOG_FILE="$STATE_DIR/cloudflared.log"
URL_FILE="$STATE_DIR/tunnel.url"
HOOK_FILE="$STATE_DIR/hook.id"

mkdir -p "$STATE_DIR"

log()  { printf '[dev-webhook] %s\n' "$*" >&2; }
die()  { log "ERROR: $*"; exit 1; }

ensure_secret() {
  [[ -f "$ENV_FILE" ]] || die ".env missing at $ENV_FILE"
  local cur
  cur=$(grep -E '^GITHUB_WEBHOOK_SECRET=' "$ENV_FILE" | head -1 | cut -d= -f2- || true)
  if [[ -z "$cur" || "$cur" == "local-dev-secret" || ${#cur} -lt 32 ]]; then
    local fresh
    fresh=$(openssl rand -hex 32)
    if grep -qE '^GITHUB_WEBHOOK_SECRET=' "$ENV_FILE"; then
      # macOS sed needs '' for -i
      sed -i '' -E "s|^GITHUB_WEBHOOK_SECRET=.*|GITHUB_WEBHOOK_SECRET=$fresh|" "$ENV_FILE"
    else
      printf 'GITHUB_WEBHOOK_SECRET=%s\n' "$fresh" >> "$ENV_FILE"
    fi
    log "rotated GITHUB_WEBHOOK_SECRET (was empty/weak/short)"
  fi
}

webhook_secret() {
  grep -E '^GITHUB_WEBHOOK_SECRET=' "$ENV_FILE" | head -1 | cut -d= -f2-
}

ensure_orchestrator() {
  if curl -fsS --max-time 3 "$ORCH_HEALTHZ" >/dev/null 2>&1; then
    log "orchestrator already up on $ORCH_HEALTHZ"
    return
  fi
  log "starting orchestrator container (docker compose up -d orchestrator)"
  (cd "$ROOT" && docker compose up -d orchestrator >/dev/null)
  for _ in $(seq 1 30); do
    if curl -fsS --max-time 3 "$ORCH_HEALTHZ" >/dev/null 2>&1; then
      log "orchestrator healthy"
      return
    fi
    sleep 2
  done
  die "orchestrator did not come up on $ORCH_HEALTHZ"
}

restart_orchestrator_if_env_changed() {
  # If GITHUB_WEBHOOK_SECRET or GITHUB_TOKEN in .env don't match the values
  # loaded into the running container, restart so the new env takes effect.
  local env_secret env_token runtime_secret runtime_token
  env_secret=$(webhook_secret)
  env_token=$(grep -E '^GITHUB_TOKEN=' "$ENV_FILE" | head -1 | cut -d= -f2- || true)
  runtime_secret=$(docker inspect grokrxiv-orchestrator \
    --format '{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null \
    | awk -F= '/^GITHUB_WEBHOOK_SECRET=/{print substr($0, index($0, "=")+1)}' \
    | head -1 || true)
  runtime_token=$(docker inspect grokrxiv-orchestrator \
    --format '{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null \
    | awk -F= '/^GITHUB_TOKEN=/{print substr($0, index($0, "=")+1)}' \
    | head -1 || true)
  local need_restart=0
  if [[ -n "$runtime_secret" && "$runtime_secret" != "$env_secret" ]]; then
    log "GITHUB_WEBHOOK_SECRET differs between container and .env; restarting"
    need_restart=1
  fi
  if [[ -n "$env_token" && "$env_token" != "$runtime_token" ]]; then
    log "GITHUB_TOKEN differs between container and .env; restarting"
    need_restart=1
  fi
  if (( need_restart )); then
    (cd "$ROOT" && docker compose up -d --force-recreate orchestrator >/dev/null)
    for _ in $(seq 1 30); do
      curl -fsS --max-time 3 "$ORCH_HEALTHZ" >/dev/null 2>&1 && break
      sleep 2
    done
  fi
}

cloudflared_alive() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid; pid=$(cat "$PID_FILE")
  ps -p "$pid" >/dev/null 2>&1
}

ensure_cloudflared() {
  if cloudflared_alive; then
    log "cloudflared already running (pid $(cat "$PID_FILE"))"
    return
  fi
  command -v cloudflared >/dev/null || die "cloudflared not installed (brew install cloudflared)"
  : > "$LOG_FILE"
  cloudflared tunnel --url "http://localhost:${ORCH_PORT}" > "$LOG_FILE" 2>&1 &
  echo $! > "$PID_FILE"
  log "started cloudflared (pid $(cat "$PID_FILE")); waiting for URL"
  local turl=""
  for _ in $(seq 1 30); do
    turl=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$LOG_FILE" | head -1 || true)
    [[ -n "$turl" ]] && break
    sleep 1
  done
  [[ -n "$turl" ]] || { tail -20 "$LOG_FILE" >&2; die "cloudflared did not print a URL"; }
  printf '%s' "$turl" > "$URL_FILE"
  # Verify reachability via public DNS — local mDNS may lag for a minute.
  for _ in $(seq 1 30); do
    local ip
    ip=$(dig @1.1.1.1 +short "${turl#https://}" 2>/dev/null | grep -E '^[0-9.]+$' | head -1)
    if [[ -n "$ip" ]] && curl -fsS --max-time 5 --resolve "${turl#https://}:443:${ip}" "$turl/healthz" >/dev/null 2>&1; then
      log "tunnel up: $turl"
      return
    fi
    sleep 2
  done
  die "tunnel $turl did not become reachable"
}

current_hook_id() {
  gh api "repos/$REPO/hooks" 2>/dev/null \
    | python3 -c "import json,sys; d=json.load(sys.stdin); [print(h['id']) for h in d if 'webhook/github' in h.get('config',{}).get('url','')]" \
    | head -1
}

upsert_hook() {
  local turl secret hook_id desired_url
  turl=$(cat "$URL_FILE")
  secret=$(webhook_secret)
  desired_url="${turl}/webhook/github"
  hook_id=$(current_hook_id || true)
  if [[ -n "$hook_id" ]]; then
    log "patching existing hook $hook_id → $desired_url"
    # -F = parsed (boolean for active); -f = string (config fields)
    gh api -X PATCH "repos/$REPO/hooks/$hook_id" \
      -F active=true \
      -f 'events[]=pull_request' \
      -f "config[url]=$desired_url" \
      -f 'config[content_type]=json' \
      -f "config[secret]=$secret" \
      -f 'config[insecure_ssl]=0' >/dev/null
  else
    log "creating new hook on $REPO"
    hook_id=$(gh api -X POST "repos/$REPO/hooks" \
      -f name=web -F active=true \
      -f 'events[]=pull_request' \
      -f "config[url]=$desired_url" \
      -f 'config[content_type]=json' \
      -f "config[secret]=$secret" \
      -f 'config[insecure_ssl]=0' \
      | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
  fi
  printf '%s' "$hook_id" > "$HOOK_FILE"
  log "hook id $hook_id → $desired_url"
}

ping_hook() {
  local hook_id; hook_id=$(cat "$HOOK_FILE")
  gh api -X POST "repos/$REPO/hooks/$hook_id/pings" >/dev/null
  log "sent ping to hook $hook_id; verify in github web ui or:"
  log "  gh api repos/$REPO/hooks/$hook_id/deliveries --jq '.[0]'"
}

cmd_up() {
  ensure_secret
  ensure_orchestrator
  restart_orchestrator_if_env_changed
  ensure_cloudflared
  upsert_hook
  ping_hook
  cmd_status
}

cmd_down() {
  if [[ -f "$HOOK_FILE" ]]; then
    local hook_id; hook_id=$(cat "$HOOK_FILE")
    if [[ -n "$hook_id" ]]; then
      log "deleting hook $hook_id"
      gh api -X DELETE "repos/$REPO/hooks/$hook_id" >/dev/null 2>&1 || true
    fi
    rm -f "$HOOK_FILE"
  else
    local hook_id; hook_id=$(current_hook_id || true)
    if [[ -n "$hook_id" ]]; then
      log "deleting orphaned hook $hook_id"
      gh api -X DELETE "repos/$REPO/hooks/$hook_id" >/dev/null 2>&1 || true
    fi
  fi
  if cloudflared_alive; then
    local pid; pid=$(cat "$PID_FILE")
    log "killing cloudflared pid $pid"
    kill "$pid" 2>/dev/null || true
    rm -f "$PID_FILE"
  fi
  rm -f "$URL_FILE"
  log "tunnel + hook torn down. orchestrator left running (use docker compose stop orchestrator)"
}

cmd_status() {
  local turl="" hook_id=""
  [[ -f "$URL_FILE" ]] && turl=$(cat "$URL_FILE")
  [[ -f "$HOOK_FILE" ]] && hook_id=$(cat "$HOOK_FILE")
  printf '\n'
  printf '  orchestrator : %s\n' "$(curl -fsS --max-time 3 "$ORCH_HEALTHZ" 2>/dev/null || echo down)"
  printf '  cloudflared  : %s\n' "$(cloudflared_alive && echo "running (pid $(cat "$PID_FILE"))" || echo down)"
  printf '  tunnel url   : %s\n' "${turl:-<none>}"
  printf '  hook id      : %s\n' "${hook_id:-<none>}"
  printf '  webhook url  : %s\n' "${turl:+${turl}/webhook/github}"
  printf '\n'
}

cmd_logs() {
  [[ -f "$LOG_FILE" ]] || die "no log at $LOG_FILE — has the tunnel been started?"
  tail -f "$LOG_FILE"
}

case "${1:-up}" in
  up)     cmd_up ;;
  down)   cmd_down ;;
  status) cmd_status ;;
  logs)   cmd_logs ;;
  *)      die "unknown subcommand: $1 (use up|down|status|logs)" ;;
esac
