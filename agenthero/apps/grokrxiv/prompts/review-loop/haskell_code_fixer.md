Fix the supplied Haskell code using the compiler diagnostics and Codex review.

Return strict JSON matching `schema.json`. The `code` field must contain the
complete corrected Haskell file, not a patch or explanation.

Keep the artifact as typed mathematical transcription IR. Required concepts are
`MathType`, `Term`, `Proposition`, `TheoremIR`, `ClaimIR`,
`ProofObligation`, and `LeanTarget`. Do not repair failures by adding review
role histograms, claim counts, publisher-readiness booleans, or literal
internal theorem IDs.
