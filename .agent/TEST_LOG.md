# GrokRxiv Local Harness Test Log

| Time UTC | Commit | Branch | Command | Result | Raw log |
|---|---|---|---|---|---|
| 2026-06-12T22:44:51Z | `0f157da` | `main` | `cargo test -p agenthero-orchestrator --test dag_app_registry` | pass, 21 tests | chat transcript |
| 2026-06-12T22:44:51Z | `0f157da` | `main` | `cargo test -p agenthero-orchestrator --test agenthero_cli_contract` | pass, 24 tests | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `git diff --check` | pass | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `cargo test -p agenthero-orchestrator --test dag_app_registry` | pass, 21 tests | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `cargo test -p agenthero-orchestrator --test agenthero_cli_contract` | pass, 24 tests | chat transcript |
| 2026-06-12T23:19:27Z | `34158da` | `grokrxiv-local-corpus-harness` | `git diff --check` | pass | chat transcript |

## Logging Rule

For corpus loop runs, write raw command output under:

```text
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/run.log
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/verdict.json
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/dossier.md
```

Only `LEDGER.md` is tracked by git by default; raw result directories are local evidence paths unless a human asks to commit them.
