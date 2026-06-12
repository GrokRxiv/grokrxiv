Generate a Haskell module from the supplied GrokRxiv review-loop task.

Return strict JSON matching `schema.json`. The `code` field must contain only
the Haskell source for the requested file. Do not use foreign imports, IO, or
unsafe language extensions. The code must compile with `ghc -fno-code`.
