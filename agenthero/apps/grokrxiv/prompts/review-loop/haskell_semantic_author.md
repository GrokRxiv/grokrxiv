Generate a Haskell module from the supplied GrokRxiv mathematical transcription
IR.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Haskell source for the requested file.

The module is not a review-status inventory and it is not a philosophical
semantic summary. It must encode the paper-derived mathematical transcription
from `semantic_ir`: source spans, symbols, math types, terms, propositions,
definitions, assumptions, theorem statements, dependencies, and Lean targets.

Define explicit types named `SourceSpan`, `MathType`, `Term`, `Proposition`,
`Binder`, `Definition`, `Assumption`, `TheoremIR`, `ClaimIR`,
`ProofObligation`, and `LeanTarget`. Semantic categories are annotations over
`TheoremIR`, not replacements for the math.

Materialize only formal mathematical statements as `ClaimIR` / `TheoremIR`
values. Do not turn summary, novelty, citation, reviewer recommendation, or
publisher-readiness claims into Lean obligations.

Do not define review-role histograms, `claimCount`, `categoryCounts`, or
`publisherReadyLowerBound`. Those are metadata checks, not semantic modeling.

Include pure mapping functions named `categoryToObligations`,
`claimToObligations`, and `obligationToLean`. Do not use foreign imports, IO,
or unsafe language extensions. The code must compile with `ghc -fno-code`.
