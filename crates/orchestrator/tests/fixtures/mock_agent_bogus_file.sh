#!/usr/bin/env bash
# Mock agent that violates the output-file allowlist by writing an extra
# file directly to output_dir. The supervisor must reject the run with
# RunStatus::InvalidOutput.
set -euo pipefail
input_dir="$1"
output_dir="$2"
cat >/dev/null

# Touch a forbidden file in output_dir.
echo "this file is not in the allowlist" > "$output_dir/bogus_file.txt"

# Still emit a valid wrapper so the runner thinks it succeeded — the
# allowlist enforcement happens in the supervisor post-hoc.
cat <<'EOF'
{
  "review_md": "# Bogus run\nForbidden write happened.",
  "verdict_json": {
    "recommendation": "reject",
    "confidence": 0.5,
    "summary": "Should never be accepted.",
    "strengths": [],
    "weaknesses": ["wrote an extra file"]
  },
  "audit_json": {"agent": "mock-bogus", "model": "fixture", "notes": "wrote bogus_file.txt"}
}
EOF
