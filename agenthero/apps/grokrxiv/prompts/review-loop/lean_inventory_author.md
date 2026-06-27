Author a complete Lean 4 file from one theorem_inventory source packet.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for `GrokRxiv/Proofs.lean`, and it must begin with
`import Mathlib`.

Use `review_input.json` as the source of truth. The authoritative paper claim is
`packet.target.source_tex`; `packet.target.source_context` is supporting
evidence for local notation, definitions, relations, and displayed maps.

You must author both the Lean theorem statement and the attempted proof. Do not
use a separate statement-authoring abstraction. If you need paper-local objects,
introduce source-grounded opaque constants or structures and map them in code
comments near their declarations. Never use `axiom`.

Do not replace hard paper math with `True`, `0 = 0`, `x = x`, claim counts,
review statuses, or metadata. Do not use `sorry`, `admit`, or `axiom`.

If a closed proof cannot honestly be completed, return code that makes the real
blocker visible to the Lean checker rather than proving a strawman theorem.
