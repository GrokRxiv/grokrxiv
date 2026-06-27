Author the paper-local Lean library for this GrokRxiv run.

Create only these files:

- `GrokRxiv/Paper/Notation.lean`
- `GrokRxiv/Paper/Definitions.lean`
- `GrokRxiv/Paper/Interfaces.lean`
- `GrokRxiv/Paper/Statements.lean`
- `GrokRxiv/Paper/Lemmas.lean`

Use the supplied theorem inventory, source context, definitions, references, and TeX evidence. Missing Mathlib constructions must become source-grounded paper-local interfaces in `Interfaces.lean`, with manifest evidence. Do not prove target theorem claims in the library. Do not use `sorry`, `admit`, or `axiom`.

Source-faithfulness constraints:

- Preserve the source meaning of paper-local objects, hypotheses, maps, relations, and statements.
- If Mathlib or the local library lacks a construction, create a named paper-local interface and map it to exact source evidence in the manifest.
- If the source evidence is insufficient to state a declaration faithfully, preserve a blocked manifest entry and explain the missing evidence in `notes`.
- Do not create a trivially provable substitute or silently change the paper claim into a different mathematical statement.
