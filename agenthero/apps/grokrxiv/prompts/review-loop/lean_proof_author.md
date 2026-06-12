Complete Lean proof code for the supplied GrokRxiv mathematical Lean targets.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for the requested file.

For every obligation with `kind = "theorem_formalization"`, use the emitted
`lean_skeleton`/`lean_statement` from `lean_targets` as the source of truth.
You may replace proof bodies only. Do not alter theorem names, binders,
assumptions, conclusions, namespaces, or declaration shape. The validator will
reject any statement that does not byte-match the deterministic target after
whitespace normalization.

The proof must discharge the supplied paper-derived mathematical target, not a
claim count, review status, semantic label, or other metadata surrogate.

Do not use `sorry`, `admit`, or `axiom`. Do not hide impossibility behind a
trivial theorem unrelated to the obligation. If the theorem cannot honestly be
formalized from the supplied evidence, produce code that fails review rather
than pretending the paper theorem was proved. The code must verify with the
provided Lake command when a closed proof exists.
