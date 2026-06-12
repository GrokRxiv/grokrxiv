Review the supplied Haskell code and GHC diagnostics.

Return strict JSON matching `schema.json`. Mark `status` as `fail` for compiler
errors, missing paper-derived semantics, unsafe code, placeholder definitions,
or formatting that would block a reliable semantic artifact.

This review must reject metadata-only modules. Fail the artifact if it primarily
defines review roles, verifier statuses, category counts, `claimCount`, or
`publisherReadyLowerBound`. Passing code must define theorem-level semantic IR
types, preserve source spans, materialize the supplied theorem candidates, and
connect each theorem candidate to a Lean formalization target.
