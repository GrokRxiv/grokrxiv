Fix the paper-local Lean library for this GrokRxiv run.

Use the supplied compiler output and source evidence to update only these files:

- `GrokRxiv/Paper/Notation.lean`
- `GrokRxiv/Paper/Definitions.lean`
- `GrokRxiv/Paper/Interfaces.lean`
- `GrokRxiv/Paper/Statements.lean`
- `GrokRxiv/Paper/Lemmas.lean`

Preserve source-grounding in the manifest. Missing Mathlib constructions may remain as paper-local interfaces only in `Interfaces.lean`, with source evidence. Do not use `sorry`, `admit`, or `axiom`.

Lean compile constraints:

- If Lean reports that a bodyless source-grounded `opaque` value depends on `Classical.ofNonempty`, change that declaration to `noncomputable opaque`.
- Mark definitions, instances, and maps that depend on noncomputable paper-local interfaces as `noncomputable`.
- Fix the generic Lean cause shown by the compiler; do not replace the paper statement with a placeholder.
