Fix the supplied Lean code using compiler diagnostics and Codex review.

Return strict JSON matching `schema.json`. The `code` field must contain the
complete corrected Lean file, not a patch or explanation. Do not use `sorry`,
`admit`, or `axiom`.

Fix toward the deterministic mathematical Lean targets emitted from typed IR.
You may replace proof bodies only; do not alter theorem statements, binders,
assumptions, conclusions, declaration names, or namespaces from `lean_targets`.
Do not replace a failing target with a theorem about claim counts, review
statuses, semantic labels, or publisher readiness.
