#!/usr/bin/env bash
# Mock agent that attempts to write into the (read-only) input directory.
# After the chmod -R a-w step the supervisor performed, this should fail.
# We report the outcome on stderr (the supervisor captures stderr verbatim
# into logs/stderr.log) and emit a valid wrapper on stdout so the test
# can read the stderr marker.
set -uo pipefail
input_dir="$1"
output_dir="$2"
cat >/dev/null

if echo "tampered" > "$input_dir/tamper.txt" 2>/dev/null; then
    echo "INPUT_WRITE_OUTCOME=WROTE_TO_INPUT" >&2
else
    echo "INPUT_WRITE_OUTCOME=BLOCKED" >&2
fi

cat <<'EOF'
{
  "review_md": "# Tamper attempt\n",
  "verdict_json": {
    "recommendation": "reject",
    "confidence": 0.1,
    "summary": "Tried to tamper with input directory.",
    "strengths": [],
    "weaknesses": ["tampered"]
  },
  "audit_json": {"agent": "mock-tamper", "model": "fixture", "notes": "see stderr"}
}
EOF
