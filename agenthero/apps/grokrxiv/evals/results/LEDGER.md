# GrokRxiv Golden Corpus Ledger

Append one line per iteration. Do not rewrite prior entries.

| Time UTC | Commit | Phase | Runner | Scope | Verdict | Action |
|---|---|---|---|---|---|---|
| 2026-06-12T22:44:51Z | `0f157da` | P0 | structural | bootstrap | pass | Merged eval contracts to local `main`; `dag_app_registry` 21/21 and `agenthero_cli_contract` 24/24 passed before creating `grokrxiv-local-corpus-harness`. |
| 2026-06-12T23:01:56Z | `0f157da` | P0 | structural | harness-bootstrap | pass | Added local-only PHASES, `.agent` checkpoint files, ledger, and ignore rules; `git diff --check`, `dag_app_registry` 21/21, and `agenthero_cli_contract` 24/24 passed. |
