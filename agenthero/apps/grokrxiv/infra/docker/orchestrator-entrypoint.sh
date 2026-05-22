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

copy_auth_file() {
  name="$1"
  copy_auth_file_to "$name" "$name"
}

copy_auth_file_to() {
  name="$1"
  target_name="$2"
  src="${AUTH_SOURCE}/${name}"
  dst="${AUTH_HOME}/${target_name}"
  if [ ! -f "$src" ]; then
    return 0
  fi
  dst_dir="$(dirname "$dst")"
  mkdir -p "$dst_dir"
  cp -p "$src" "$dst"
  chown "${RUN_USER}:${RUN_USER}" "$dst_dir"
  chmod 700 "$dst_dir" 2>/dev/null || true
  chown "${RUN_USER}:${RUN_USER}" "$dst"
  chmod 600 "$dst" 2>/dev/null || true
}

write_gemini_oauth_settings() {
  if [ ! -f "${AUTH_HOME}/.gemini/oauth_creds.json" ]; then
    return 0
  fi

  auth_type="${GROKRXIV_GEMINI_AUTH_TYPE:-oauth-personal}"
  case "$auth_type" in
    oauth-personal) ;;
    *) auth_type="oauth-personal" ;;
  esac

  dst="${AUTH_HOME}/.gemini/settings.json"
  dst_dir="$(dirname "$dst")"
  mkdir -p "$dst_dir"
  cat >"$dst" <<EOF
{
  "selectedAuthType": "${auth_type}",
  "security": {
    "auth": {
      "selectedType": "${auth_type}"
    }
  }
}
EOF
  chown "${RUN_USER}:${RUN_USER}" "$dst_dir" "$dst"
  chmod 700 "$dst_dir" 2>/dev/null || true
  chmod 600 "$dst" 2>/dev/null || true
}

copy_auth_item ".claude.json"
copy_auth_file_to ".claude/docker-claude-code-credentials.secret" ".claude/.credentials.json"
copy_auth_file_to ".claude/docker-claude-code-credentials.secret" ".claude/credentials/default.json"
copy_auth_file ".codex/auth.json"
copy_auth_file ".gemini/oauth_creds.json"
copy_auth_file ".gemini/google_accounts.json"
write_gemini_oauth_settings

if [ "${GROKRXIV_REQUIRE_CLI_AUTH:-0}" = "1" ]; then
  missing=""
  [ -f "${AUTH_HOME}/.claude/.credentials.json" ] || [ -f "${AUTH_HOME}/.claude/credentials/default.json" ] || missing="${missing} claude"
  [ -f "${AUTH_HOME}/.codex/auth.json" ] || missing="${missing} codex"
  [ -f "${AUTH_HOME}/.gemini/oauth_creds.json" ] || missing="${missing} gemini"
  if [ -n "$missing" ]; then
    echo "missing CLI auth material:${missing}" >&2
    exit 1
  fi
fi

exec gosu "$RUN_USER" "$@"
