Author the Lean 4 theorem statement for one paper theorem before proof search.

Return strict JSON matching `schema.json`.

Your source of truth is the theorem/source packet in `review_input.json`:

- exact `source_tex`
- bounded `source_context` from the same TeX source file, when present
- paper statement text
- label / source claim id / section
- dependencies and nearby paper context
- paper blueprint/entity context, when present
- typed-IR, when present

The typed-IR is scaffolding only. Use it to understand likely objects, binders,
and dependencies, but do not treat it as authoritative when it conflicts with
the source TeX. The source TeX is authoritative. Use `source_context` only to
resolve paper-local notation, surrounding definitions, numbered relations,
displayed maps, and referenced objects needed to state the theorem faithfully.
Do not turn unrelated context into extra hypotheses or conclusions.

When `blueprint_context` is present, use it as supporting evidence for
paper-local entities, Lean names, source spans, and already generated
`FormalInterfaces.lean` declarations. Do not let the blueprint override
`source_tex`; unresolved blueprint entities must stay unresolved instead of
being invented.

Create:

1. Lean declarations/binders/context needed for the theorem statement.
2. The theorem/lemma header using the exact supplied `lean_declaration` name.
   Put this in `lean_statement` ending with `:= by`, but do not include any
   proof body lines.
3. A symbol map that maps every opaque Lean symbol you introduce back to exact
   source TeX.

Do not include `import` commands in `lean_context`; the Lean harness supplies
imports before your context.

If you need uninterpreted paper-local objects or predicates, introduce them with
`opaque` declarations or variables. Never use `axiom`.

Do not prove the theorem in this role. Return only `lean_context`,
`lean_statement`, `symbol_map`, and the required status fields. Do not use
`sorry`, `admit`, or `axiom`.

Never replace hard paper math with `True`, `0 = 0`, `x = x`, claim counts,
review statuses, or metadata. If a faithful Lean statement cannot be authored
from the supplied evidence, set `status` to `not_faithfully_formalizable`, set
`lean_statement` to null, and explain the blocker in `unsupported_reason`.
