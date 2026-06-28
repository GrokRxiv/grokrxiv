<!-- BEGIN grokrxiv-skills v0.1.0 -->
# GrokRxiv codex review agent — strict JSON output

This block is for GrokRxiv schema-bound peer-review roles only. It applies
when the prompt supplies an explicit JSON output schema for one of the review
roles listed below.

Do not apply this block to Lean formalization, theorem statement authoring,
`Proofs.lean` generation, source-to-Lean debugging, code-editing work, or any
other GrokRxiv role that asks for files, patches, diagnostics, or normal prose.
A role tag by itself is not enough to activate this review-output contract.

When invoked with `--output-schema`, follow the schema literally:

- The schema is the contract. Required properties are required.
- Enum values use the exact listed strings (case-sensitive). No
  paraphrasing — `"High"` is not a member of `{"high","medium","low"}`.
- Arrays of objects contain OBJECTS, not free-form strings.
- Numeric fields are numbers (`0.65`), not strings (`"0.65"`).
- The schema is closed (`additionalProperties: false`). Do not add fields
  that are not declared.
- Nullable fields (`"type": ["string","null"]`) must still appear in the
  output, set to `null` when no value applies.

For schema-bound GrokRxiv review roles, follow the per-role shape:

| Role | Top-level required fields |
|------|---------------------------|
| `summary` | `tldr`, `plain_language_summary`, `key_contributions[]`, `audience` |
| `technical_correctness` | `claims[]`, `overall_correctness`, `confidence` |
| `novelty` | `novelty_score`, `related_work[]`, `missing_prior_art[]`, `verdict`, `confidence` |
| `reproducibility` | `code_availability`, `code_url`, `data_availability`, `data_url`, `environment`, `concerns[]`, `reproducibility_score`, `confidence` |
| `citation` | `entries[]`, `missing_references[]`, `summary`, `confidence` |
| `meta_reviewer` | `summary`, `strengths[]`, `weaknesses[]`, `questions[]`, `recommendation`, `confidence` |

The orchestrator validates your output against the schema. If validation
fails, you get one corrective retry — do not waste it on prose. Emit raw
JSON; the first character is `{`.

For the `novelty` role specifically: `related_work[]` items are objects
with `{citation_key, title, relation, delta}`. Do NOT invent `authors`,
`publication`, `url`, or `year` fields — they are not in the schema.

For the `citation` role specifically: each `entries[]` item is an object
with `{citation, exists, resolved_doi, resolved_url, relevance, notes,
explanation}`, NOT a raw citation string.
<!-- END grokrxiv-skills v0.1.0 -->
