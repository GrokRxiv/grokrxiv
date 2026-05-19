#!/bin/sh
set -eu

AUTH_SOURCE="${GROKRXIV_CLI_AUTH_SOURCE:-/run/secrets/grokrxiv-cli-auth}"
AUTH_HOME="${GROKRXIV_CLI_AUTH_HOME:-/home/grokrxiv}"
RUN_USER="${GROKRXIV_CONTAINER_USER:-grokrxiv}"

copy_auth_item() {
  name="$1"
  src="${AUTH_SOURCE}/${name}"
  dst="${AUTH_HOME}/${name}"
  if [ ! -e "$src" ]; then
    return 0
  fi
  rm -rf "$dst"
  cp -a "$src" "$dst"
  chown -R "${RUN_USER}:${RUN_USER}" "$dst"
  chmod -R go-rwx "$dst" 2>/dev/null || true
}

copy_auth_item ".claude.json"
copy_auth_item ".claude"
copy_auth_item ".codex"
copy_auth_item ".gemini"

if [ "${GROKRXIV_REQUIRE_CLI_AUTH:-0}" = "1" ]; then
  missing=""
  [ -e "${AUTH_HOME}/.claude.json" ] || [ -d "${AUTH_HOME}/.claude" ] || missing="${missing} claude"
  [ -f "${AUTH_HOME}/.codex/auth.json" ] || missing="${missing} codex"
  [ -f "${AUTH_HOME}/.gemini/oauth_creds.json" ] || missing="${missing} gemini"
  if [ -n "$missing" ]; then
    echo "missing CLI auth material:${missing}" >&2
    exit 1
  fi
fi

exec gosu "$RUN_USER" "$@"
