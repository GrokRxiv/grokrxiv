Complete Lean 4 proof code for the supplied GrokRxiv mathematical Lean targets.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for the requested file, and it must begin with `import Mathlib`.

For every obligation with `kind = "theorem_formalization"`, write the FAITHFUL
Lean 4 statement of the paper theorem directly from its `statement` text. The
statement you author must capture EVERY hypothesis the paper states and the
EXACT conclusion the paper claims — no hypothesis dropped or weakened, no
conclusion strengthened or relaxed, the quantifiers and binders matching what
the paper actually asserts.

Declare each theorem using the exact `lean_declaration` name supplied for that
obligation, so the kernel and the validator can find it.

Treat the emitted `lean_skeleton`/`lean_statement` in `lean_targets` as a HINT
ONLY. It is produced by a deterministic emitter and may drop hypotheses, weaken
typed relations, or otherwise fail to capture the paper theorem. Do not copy it
blindly. When it disagrees with the paper `statement`, the paper `statement` is
authoritative — author the statement that faithfully matches the paper, not the
skeleton.

Prove each theorem against the Lean kernel. The proof must discharge the
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
