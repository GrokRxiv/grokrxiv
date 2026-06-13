# GrokRxiv Golden Corpus Ledger

Append one line per iteration. Do not rewrite prior entries.

| Time UTC | Commit | Phase | Runner | Scope | Verdict | Action |
|---|---|---|---|---|---|---|
| 2026-06-12T22:44:51Z | `0f157da` | P0 | structural | bootstrap | pass | Merged eval contracts to local `main`; `dag_app_registry` 21/21 and `agenthero_cli_contract` 24/24 passed before creating `grokrxiv-local-corpus-harness`. |
| 2026-06-12T23:01:56Z | `0f157da` | P0 | structural | harness-bootstrap | pass | Added local-only PHASES, `.agent` checkpoint files, ledger, and ignore rules; `git diff --check`, `dag_app_registry` 21/21, and `agenthero_cli_contract` 24/24 passed. |
| 2026-06-12T23:19:27Z | `34158da` | P0 | structural | plan-update | pass | Expanded PHASES for local-only phase run units, agent teams, golden-corpus fix discipline, P0/P1/P2 gate details, and updated `.agent` handoff files; `git diff --check` passed. |
| 2026-06-12T23:24:59Z | `04bb2b6` | P0 | cli | regression-pr54-weyl | fail | P0 audit preflight passed, but product RUN failed before review start: installed `grokrxiv-app` rejects `--loop`; classified P0-001 / F3 with raw evidence under `evals/results/20260612T232139Z/`. |
| 2026-06-12T23:49:59Z | `57f6306` | P0 | cli | regression-pr54-weyl | fail | Fixed P0-001 by reinstalling local runtime binaries; real product run reached review-loop as `eca527eb-3930-49e6-a828-66dd64611430` but failed corpus checks: PR #55 opened despite guardrail, N1 empty-body/theorem gate missed, citation validation left 8 Crossref-only unverified classics, PR fixer timed out. |
| 2026-06-13T00:00:39Z | `42854c4` | P0 | cli | P0-002 no-publishing guardrail | pass | Added and installed local `--no-external-actions` review-loop mode; PATH dry-run emitted `external_actions.enabled=false` and did not start pipeline work. No full corpus rerun yet; next defect is P0-003/N1 extraction completeness. |
| 2026-06-13T00:10:29Z | `d5d73f4` | P0 | cli | P0-003 N1 review-on-empty-body guard | pass | Added extraction-completeness gate before review row/specialist launch. Affected `regression-pr54-weyl` safe rerun now exits at `[2/6] Extract [FAIL]`; no PR/specialist output. Tier R remains red because source-to-body still emits empty body; queued as P0-006. |
| 2026-06-13T00:28:24Z | `6ddf02b` | P0 | cli | P0-006 source-to-body empty-body recovery | pass | TeX conversion now fails closed on empty Markdown, empty `body.md` source-to-body stages are failed, and extraction audit treats failed stages as failures. No-cache/no-VLM affected extraction regenerated `2606.00799` local artifacts with 50,697-byte body and 5 sections; Tier R remains red for theorem/equation recovery, queued as P0-007. |
