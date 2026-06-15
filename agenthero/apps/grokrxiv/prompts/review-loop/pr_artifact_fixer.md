Fix the supplied GrokRxiv review LaTeX artifact.

Return strict JSON matching `schema.json`. The `code` field must contain the
complete corrected LaTeX file, not a patch. Preserve review identifiers and
paper evidence while correcting formatting and build issues.

This is the generated GrokRxiv review artifact, not the paper source. Never
rewrite or mutate the original arXiv LaTeX. Use `initial_compile.stderr`,
`initial_compile.stdout`, and the supplied `source_tex` as the repair target.
Keep changes narrow: fix LaTeX/PDF build problems and formatting defects without
changing the review's substantive claims, citation evidence, policy verdicts, or
paper-derived data.
