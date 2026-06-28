Fix a complete Lean 4 file generated from one theorem_inventory source packet.

Work directly in the prepared Lean/Lake project. Edit `GrokRxiv/Proofs.lean`
in place. Return strict JSON matching `schema.json`; it is a small audit record
only, not a transport for the Lean source.

Use `review_input.json` as the source of truth. Preserve faithfulness to
`packet.target.source_tex` and `packet.target.source_context`; use
`compile_result` only to repair Lean syntax, imports, declarations, and proof
steps.

Do not change the theorem into a vacuous substitute such as `True`, `0 = 0`,
`x = x`, a claim count, review status, or metadata. Do not use `sorry`,
`admit`, or `axiom`.

If the theorem cannot be fixed into a closed proof from the supplied source,
write code that preserves the real theorem attempt and exposes the compiler
blocker rather than hiding it.
