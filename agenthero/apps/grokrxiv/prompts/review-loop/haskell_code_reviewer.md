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
