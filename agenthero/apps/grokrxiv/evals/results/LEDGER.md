# GrokRxiv Golden Corpus Ledger

Append one line per iteration. Do not rewrite prior entries.

| Time UTC | Commit | Phase | Runner | Scope | Verdict | Action |
|---|---|---|---|---|---|---|
| 2026-06-12T22:44:51Z | `0f157da` | P0 | structural | bootstrap | pass | Merged eval contracts to local `main`; `dag_app_registry` 21/21 and `agenthero_cli_contract` 24/24 passed before creating `grokrxiv-local-corpus-harness`. |
| 2026-06-12T23:01:56Z | `0f157da` | P0 | structural | harness-bootstrap | pass | Added local-only PHASES, `.agent` checkpoint files, ledger, and ignore rules; `git diff --check`, `dag_app_registry` 21/21, and `agenthero_cli_contract` 24/24 passed. |
| 2026-06-12T23:19:27Z | `34158da` | P0 | structural | plan-update | pass | Expanded PHASES for local-only phase run units, agent teams, golden-corpus fix discipline, P0/P1/P2 gate details, and updated `.agent` handoff files; `git diff --check` passed. |
| 2026-06-12T23:24:59Z | `04bb2b6` | P0 | cli | regression-pr54-weyl | fail | P0 audit preflight passed, but product RUN failed before review start: installed `grokrxiv-app` rejects `--loop`; classified P0-001 / F3 with raw evidence under `evals/results/20260612T232139Z/`. |
| 2026-06-12T23:49:59Z | `57f6306` | P0 | cli | regression-pr54-weyl | fail | Fixed P0-001 by reinstalling local runtime binaries; real product run reached review-loop as `eca527eb-3930-49e6-a828-66dd64611430` but failed corpus checks: PR #55 opened despite guardrail, N1 empty-body/theorem gate missed, citation validation left 8 Crossref-only unverified classics, PR fixer timed out. |
