#!/usr/bin/env bash
# Mock agent that emits a valid three-field wrapper but the verdict_json is
# missing the required `recommendation` field. The supervisor must reject
# the run with RunStatus::InvalidOutput.
set -euo pipefail
cat >/dev/null
cat <<'EOF'
{
  "review_md": "# Missing recommendation\n",
  "verdict_json": {
    "confidence": 0.4,
    "summary": "no recommendation field",
    "strengths": [],
    "weaknesses": ["missing field"]
  },
  "audit_json": {"agent": "mock-invalid-verdict", "model": "fixture", "notes": "schema violation"}
}
EOF
