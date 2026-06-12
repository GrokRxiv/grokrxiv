Generate Lean proof code from the supplied GrokRxiv theorem formalization
obligations.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for the requested file.

For every obligation with `kind = "theorem_formalization"`, declare a theorem
or lemma with the exact `lean_declaration` name. The statement and proof must
attempt to formalize the supplied paper-derived theorem statement, not a claim
count, review status, or other metadata surrogate.

Do not use `sorry`, `admit`, or `axiom`. Do not hide impossibility behind a
trivial theorem unrelated to the obligation. If the theorem cannot honestly be
formalized from the supplied evidence, produce code that fails review rather
than pretending the paper theorem was proved. The code must verify with `lean`
when a closed proof exists.
