Fix the supplied Lean code using compiler diagnostics and Codex review.

Return strict JSON matching `schema.json`. The `code` field must contain the
complete corrected Lean file, not a patch or explanation. Do not use `sorry`,
`admit`, or `axiom`.

Fix toward the mathematical Lean targets emitted from the Haskell IR. Do not
replace a failing target with a theorem about claim counts, review statuses,
semantic labels, or publisher readiness.
