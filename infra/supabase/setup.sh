#!/usr/bin/env bash
# GrokRxiv — local Supabase bring-up.
#
# Idempotent: safe to re-run. Starts the local Supabase stack, applies the
# Postgres migrations, and ensures the three Storage buckets exist.
#
# Requires: `supabase` CLI (https://supabase.com/docs/guides/cli), `psql`.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"
MIGRATIONS_DIR="${REPO_ROOT}/migrations"

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

echo "==> applying migrations from ${MIGRATIONS_DIR}"
for f in $(ls "${MIGRATIONS_DIR}"/*.sql | sort); do
  echo "    -> $(basename "${f}")"
  psql "${DB_URL}" -v ON_ERROR_STOP=1 -f "${f}"
done

echo "==> ensuring storage buckets exist"
# Bucket policy:
#   pdfs    — PRIVATE. Source uploads, never publicly listed.
#   bundles — PUBLIC-READ. Only contains artifacts derived from
#             approved/published reviews.
#   renders — PUBLIC-READ. Only contains artifacts derived from
#             approved/published reviews.
for bucket in pdfs bundles renders; do
  if ! supabase storage list 2>/dev/null | grep -q -E "(^|/)${bucket}( |$)"; then
    case "${bucket}" in
      pdfs)    supabase storage create-bucket "${bucket}" || true ;;
      bundles) supabase storage create-bucket "${bucket}" --public || true ;;
      renders) supabase storage create-bucket "${bucket}" --public || true ;;
    esac
  fi
done

# Enforce desired visibility EVERY run — `pdfs` must remain private even if a
# previous run accidentally created it as public.
psql "${DB_URL}" -v ON_ERROR_STOP=1 <<'SQL'
update storage.buckets set public = false where id = 'pdfs';
update storage.buckets set public = true  where id in ('bundles','renders');
SQL

echo "==> done. Studio: $(supabase status --output env | awk -F= '/^STUDIO_URL=/{print $2}' | tr -d '"')"
