Generate Lean proof code from the supplied GrokRxiv proof obligations.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Lean source for the requested file. Do not use `sorry`, `admit`, or `axiom`.
The code must verify with `lean`.
