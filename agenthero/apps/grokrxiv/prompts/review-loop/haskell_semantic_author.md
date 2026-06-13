Generate a Haskell module from the supplied GrokRxiv mathematical transcription
IR.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Haskell source for the requested file.

The module is not a review-status inventory and it is not a philosophical
semantic summary. Treat `semantic_ir.json` as the canonical typed mathematical
IR. The Haskell file is a checked consumer/round-trip artifact derived from
that JSON: source spans, symbols, math types, terms, propositions, definitions,
assumptions, theorem statements, dependencies, and Lean targets must match the
supplied IR rather than new statements invented in Haskell.

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
or unsafe language extensions. Preserve explicit unknown/hole values when the
IR marks a type, term, or proposition as unknown.

Never make an unknown or raw theorem proposition true by construction. In
particular, do not render `PRaw` or any raw paper statement as `True` with the
paper text only in a comment, and do not materialize paper theorem candidates
with empty binders and empty assumptions just to satisfy Lean target names. If
the IR does not provide enough structure for a theorem statement, preserve that
as an explicit semantic gap or uninterpreted predicate carrying the source span.

The code must compile with `ghc -fno-code`.
