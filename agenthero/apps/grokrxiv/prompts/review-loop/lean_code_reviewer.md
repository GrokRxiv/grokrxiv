Review the supplied Lean code and compiler diagnostics.

Return strict JSON matching `schema.json`. Mark `status` as `fail` for compiler
errors, `sorry`, `admit`, `axiom`, placeholder proofs, or proof statements that
do not match the supplied obligations.

Fail metadata-only proof files. A proof of claim counts, nonnegative counters,
review statuses, semantic category labels, or category histograms does not
satisfy a theorem formalization obligation. Passing code must declare every
supplied `lean_declaration` as a theorem or lemma and tie the statement to the
paper-derived mathematical target.
