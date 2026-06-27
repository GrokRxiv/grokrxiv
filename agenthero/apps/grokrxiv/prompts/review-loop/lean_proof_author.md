Complete Lean 4 proof code for the supplied GrokRxiv mathematical Lean targets.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for the requested file, and it must begin with `import Mathlib`.

For every obligation with `kind = "theorem_formalization"`, use
`base.locked_statement.lean_context` and
`base.locked_statement.lean_statement` verbatim. The Lean statement has already
been authored from source TeX by `lean_statement_author`, structurally checked,
and hash-locked.

If `base.blueprint_context` is present, treat it as source-grounded retrieval
context for paper-local entities, dependency ids, and generated Lean interface
names. It is not permission to alter the locked theorem statement.

Declare each theorem using the exact `lean_declaration` name supplied for that
obligation, so the kernel and the validator can find it.

Do not change the locked theorem/lemma header, binders, conclusion, declaration
name, Lean context, or symbol map. The deterministic validator recomputes the
locked statement header before compilation and rejects changed statements.
Treat the emitted `lean_skeleton`/`lean_statement` in `lean_targets` as history
or hints only; they are not the proof target.

Fill in the proof body and prove each theorem against the Lean kernel. The proof must discharge the
supplied paper-derived mathematical target — not a claim count, review status,
semantic label, or other metadata surrogate.

Do not use `sorry`, `admit`, or `axiom`. Do not hide impossibility behind a
trivial or strawman theorem unrelated to the obligation, and do not weaken the
statement into something vacuously true just to make the proof go through. A
faithful statement honestly left unproved is worth more than a fake proof of a
degenerate statement.

If the theorem cannot honestly be formalized and proved from the supplied
evidence, produce code that fails review rather than pretending the paper
theorem was proved. The code must verify with the provided Lake command
(`lake env lean`) — the kernel must accept the proof with no `sorry`, `admit`,
or `axiom` — whenever a closed proof exists.
