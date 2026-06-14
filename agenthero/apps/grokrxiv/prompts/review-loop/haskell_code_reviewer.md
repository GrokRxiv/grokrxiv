Review the supplied Haskell code and GHC diagnostics.

Return strict JSON matching `schema.json`. Mark `status` as `fail` for compiler
errors, missing paper-derived mathematical transcription, unsafe code,
placeholder definitions, or formatting that would block a reliable IR artifact.

This review must reject metadata-only modules. Fail the artifact if it primarily
defines review roles, verifier statuses, category counts, `claimCount`, or
`publisherReadyLowerBound`.

Passing code must define typed mathematical IR types (`MathType`, `Term`,
`Proposition`, `TheoremIR`, `ClaimIR`) and mapping functions
(`categoryToObligations`, `claimToObligations`, `obligationToLean`). It must
preserve source spans, model assumptions/definitions, and connect formal math
statements to Lean targets. It must not require literal internal theorem IDs;
Lean declaration names are the stable proof targets.

Use `semantic_ir.theorem_candidates`, `semantic_ir.definitions`, and
`semantic_ir.assumptions` as the only canonical formal sources. Do not fail an
artifact just because summarized `claims`, `knowledge_graph`, or
`nonformal_review_claims` counts are omitted from Haskell. Those review
artifacts must not be backfilled into `ClaimIR`. If
`semantic_ir.theorem_candidates` is empty, empty `theoremTargets`, `claims`, and
proof obligations are correct when the module preserves the explicit semantic
limitations.

Fail any artifact that imports omitted summary, novelty, citation,
meta-reviewer, reproducibility, recommendation, policy, or knowledge-graph
metadata into Haskell claims or proof obligations.

Fail any artifact that renders a raw or unknown paper theorem proposition as
`True` with the source text only in a comment, or that maps paper theorem
candidates to empty binders and empty assumptions without canonical IR support.
Such code is compiler-valid but unfaithful: it turns theorem-level obligations
into metadata comments over tautologies.
