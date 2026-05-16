#!/usr/bin/env bash
# Mock agent worker for FP-RPT3d MVP integration tests.
#
# argv[1] = input_dir   (read-only)
# argv[2] = output_dir  (the supervisor materialises files there from our stdout)
# argv[3] = prompt_path (input_dir/prompt.md)
# stdin   = the rendered prompt
#
# We emit the canonical three-field JSON wrapper on stdout. The supervisor —
# not this script — writes review.md / verdict.json / audit.json into
# output_dir.
set -euo pipefail
# Drain stdin so the parent doesn't get a SIGPIPE.
cat >/dev/null

cat <<'EOF'
{
  "review_md": "# Mock review\n\nThis is a fixture-generated review. The paper proposes X and demonstrates Y. The supervisor pipeline is exercised end-to-end by this script.",
  "verdict_json": {
    "recommendation": "minor_revision",
    "confidence": 0.72,
    "summary": "Reasonable contribution; needs clarifications on the experimental setup.",
    "strengths": ["Clear problem statement", "Solid empirical methodology"],
    "weaknesses": ["Limited ablations", "Missing comparison to prior art"]
  },
  "audit_json": {
    "agent": "mock-success",
    "model": "fixture",
    "notes": "Deterministic fixture used by supervisor_runner_mvp integration tests."
  }
}
EOF
