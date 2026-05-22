#!/usr/bin/env bash
# GrokRxiv — local Supabase bring-up.
#
# Idempotent: safe to re-run. Starts the local Supabase stack and applies the
# AgentHero platform migrations followed by GrokRxiv app migrations.
#
# Requires: `supabase` CLI (https://supabase.com/docs/guides/cli), `psql`.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_ROOT="$(cd "${HERE}/../.." && pwd)"
REPO_ROOT="$(cd "${APP_ROOT}/../../.." && pwd)"
PLATFORM_MIGRATIONS_DIR="${REPO_ROOT}/agenthero/migrations"
APP_MIGRATIONS_DIR="${APP_ROOT}/migrations"

cd "${REPO_ROOT}"

if ! command -v supabase >/dev/null 2>&1; then
  echo "error: supabase CLI not found on PATH" >&2
  exit 127
fi

echo "==> starting supabase stack (idempotent)"
supabase start

DB_URL="$(supabase status --output env | awk -F= '/^DB_URL=/{print $2}' | tr -d '"')"
if [[ -z "${DB_URL}" ]]; then
  echo "error: could not read DB_URL from supabase status" >&2
  exit 1
fi

echo "==> applying platform migrations from ${PLATFORM_MIGRATIONS_DIR}"
for f in $(ls "${PLATFORM_MIGRATIONS_DIR}"/*.sql 2>/dev/null | sort); do
  echo "    -> $(basename "${f}")"
  psql "${DB_URL}" -v ON_ERROR_STOP=1 -f "${f}"
done

echo "==> applying GrokRxiv migrations from ${APP_MIGRATIONS_DIR}"
for f in $(ls "${APP_MIGRATIONS_DIR}"/*.sql | sort); do
  echo "    -> $(basename "${f}")"
  psql "${DB_URL}" -v ON_ERROR_STOP=1 -f "${f}"
done

echo "==> storage buckets are managed by GrokRxiv migrations"

echo "==> done. Studio: $(supabase status --output env | awk -F= '/^STUDIO_URL=/{print $2}' | tr -d '"')"
