#!/usr/bin/env bash
# maybe-publish-research.sh
#
# Stop-hook: regenerate HTML artifacts for any research/*.md or
# ~/.claude/plans/*.md that is newer than its rendered .html counterpart.
#
# Idempotent: if nothing is stale, exits 0 silently.
# Skips silently if the build pipeline's node_modules aren't installed yet.

set -uo pipefail

# Resolve repo root (this script lives at .claude/hooks/ under the repo)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT" || exit 0

# Skip if build dependencies haven't been installed yet (first-run, etc.)
if [ ! -d "research/_template/node_modules" ]; then
  exit 0
fi

# Skip if the build script itself is missing (Track B not yet landed)
if [ ! -f "research/_template/build.mjs" ]; then
  exit 0
fi

# Collect candidates
PLAN_DIR="${HOME}/.claude/plans"
TO_BUILD=()

check() {
  local md="$1"
  local base
  base="$(basename "$md" .md)"
  local html="research/${base}.html"

  # Build if .html missing or older than .md
  if [ ! -f "$html" ] || [ "$md" -nt "$html" ]; then
    TO_BUILD+=("$md")
  fi
}

# Scan research/
shopt -s nullglob
for md in research/*.md; do
  check "$md"
done

# Scan docs/ — fix/enhancement writeups produced during real-paper tests
# get published to HTML on the same pipeline.
for md in docs/*.md; do
  check "$md"
done

# Scan plans dir — but only grokrxiv plans (fp[N]-*.md and rpt[N]-*.md
# conventions). The plans dir at ~/.claude/plans is global across every
# Claude Code project; without this filter we'd publish unrelated plans
# into this repo's research/.
if [ -d "$PLAN_DIR" ]; then
  for md in "$PLAN_DIR"/fp[0-9]*-*.md "$PLAN_DIR"/rpt[0-9]*-*.md; do
    [ -e "$md" ] || continue
    check "$md"
  done
fi

if [ "${#TO_BUILD[@]}" -eq 0 ]; then
  # Nothing stale; quiet exit
  exit 0
fi

# Build each
errors=0
for md in "${TO_BUILD[@]}"; do
  if ! node research/_template/build.mjs "$md" >/dev/null 2>&1; then
    echo "[publish-plan] build failed for: $md" >&2
    errors=$((errors + 1))
  fi
done

# Refresh search index marker for the Next.js viewer
mkdir -p research/site/lib
date -u +%s > research/site/lib/.search-index-dirty

if [ "$errors" -gt 0 ]; then
  echo "[publish-plan] $errors of ${#TO_BUILD[@]} builds failed" >&2
  exit 1
fi

echo "[publish-plan] regenerated ${#TO_BUILD[@]} HTML artifact(s)"
exit 0
