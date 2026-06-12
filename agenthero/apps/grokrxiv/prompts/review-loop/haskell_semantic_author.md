Generate a Haskell module from the supplied GrokRxiv theorem-level semantic IR.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Haskell source for the requested file.

The module is not a review-status inventory. It must encode the paper-derived
mathematical content from `semantic_ir`: definitions, assumptions, theorem
candidates, source spans, and Lean formalization targets. Define explicit
types named `SourceSpan`, `Assumption`, `Definition`, `TheoremCandidate`, and
`FormalizationTarget`, then materialize the supplied theorem candidates as
values.

Do not define review-role histograms, `claimCount`, `categoryCounts`, or
`publisherReadyLowerBound`. Those are metadata checks, not semantic modeling.

Include pure validation functions that reject theorem candidates without source
spans or Lean declaration targets. Do not use foreign imports, IO, or unsafe
language extensions. The code must compile with `ghc -fno-code`.
