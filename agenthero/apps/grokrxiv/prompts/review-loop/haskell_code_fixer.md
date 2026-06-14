Fix the supplied Haskell code using the compiler diagnostics and Codex review.

Return strict JSON matching `schema.json`. The `code` field must contain the
complete corrected Haskell file, not a patch or explanation.

Keep the artifact as typed mathematical transcription IR. Required concepts are
`MathType`, `Term`, `Proposition`, `TheoremIR`, `ClaimIR`,
`ProofObligation`, and `LeanTarget`. Do not repair failures by adding review
role histograms, claim counts, publisher-readiness booleans, or literal
internal theorem IDs.

Do not repair an empty `theoremTargets` list by importing summary, novelty,
citation, meta-reviewer, reproducibility, recommendation, policy, or
knowledge-graph metadata. If `semantic_ir.theorem_candidates` is empty, preserve
the semantic limitation and keep `claims` / proof obligations empty.

Do not repair missing theorem targets by collapsing theorem conclusions to
`PRaw` rendered as `True` with the paper text in a comment. Do not use empty
binders and empty assumptions for paper theorem candidates unless the canonical
IR explicitly says the theorem is nullary and assumption-free. When the paper
text cannot be faithfully structured, represent it as an explicit semantic gap
or uninterpreted predicate with source-span provenance rather than a tautology.
