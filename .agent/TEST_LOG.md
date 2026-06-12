# GrokRxiv Local Harness Test Log

| Time UTC | Commit | Branch | Command | Result | Raw log |
|---|---|---|---|---|---|
| 2026-06-12T22:44:51Z | `0f157da` | `main` | `cargo test -p agenthero-orchestrator --test dag_app_registry` | pass, 21 tests | chat transcript |
| 2026-06-12T22:44:51Z | `0f157da` | `main` | `cargo test -p agenthero-orchestrator --test agenthero_cli_contract` | pass, 24 tests | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `git diff --check` | pass | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `cargo test -p agenthero-orchestrator --test dag_app_registry` | pass, 21 tests | chat transcript |
| 2026-06-12T23:01:56Z | `0f157da` | `grokrxiv-local-corpus-harness` | `cargo test -p agenthero-orchestrator --test agenthero_cli_contract` | pass, 24 tests | chat transcript |
| 2026-06-12T23:19:27Z | `34158da` | `grokrxiv-local-corpus-harness` | `git diff --check` | pass | chat transcript |
| 2026-06-12T23:21:39Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `agh doctor` | pass, exit 0 | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/preflight-agh-doctor.log` |
| 2026-06-12T23:21:39Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `agh --version` | pass, exit 0, `agh 0.1.0` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/preflight-agh-version.log` |
| 2026-06-12T23:21:39Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `ghc --version` | pass, exit 0, `9.14.1` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/preflight-ghc-version.log` |
| 2026-06-12T23:21:39Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `lake --version` | pass, exit 0, Lean `4.30.0` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/preflight-lake-version.log` |
| 2026-06-12T23:21:39Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `lean --version` | pass, exit 0, `4.30.0` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/preflight-lean-version.log` |
| 2026-06-12T23:22:50Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `agh app run grokrxiv review arxiv:2606.00799 --loop --debug --json` | fail, exit 1, installed runtime rejects `--loop` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run.log` |
| 2026-06-12T23:23:43Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json` | fail, exit 1, installed runtime rejects `--loop` | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-url.log` |
| 2026-06-12T23:23:30Z | `04bb2b6` | `grokrxiv-local-corpus-harness` | `cargo run --manifest-path agenthero/apps/grokrxiv/crates/orchestrator/Cargo.toml --quiet --bin grokrxiv-app -- --json --dry-run review https://arxiv.org/abs/2606.00799 --loop --debug` | pass, exit 0, source runtime emits review-loop stage plan | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/runtime-source-url-dry-run.log` |
| 2026-06-12T23:26:50Z | `57f6306` | `grokrxiv-local-corpus-harness` | `cargo install --path agenthero/apps/grokrxiv/crates/orchestrator --bin grokrxiv-app --force --locked` | pass | chat transcript |
| 2026-06-12T23:27:00Z | `57f6306` | `grokrxiv-local-corpus-harness` | `cargo install --path agenthero/apps/grokrxiv/rust --bin agenthero-dag-app-grokrxiv --force --locked` | pass | chat transcript |
| 2026-06-12T23:27:06Z | `57f6306` | `grokrxiv-local-corpus-harness` | `agh --dry-run app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json` | pass, exit 0, product surface emits review-loop stage plan | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/product-dry-run-after-install.log` |
| 2026-06-12T23:47:00Z | `57f6306` | `grokrxiv-local-corpus-harness` | `agh app run grokrxiv review https://arxiv.org/abs/2606.00799 --loop --debug --json` | command exit 0, review-loop deterministic fail, PR #55 opened | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/run-after-install.log` |
| 2026-06-12T23:48:15Z | `57f6306` | `grokrxiv-local-corpus-harness` | `ghc -fno-code SemanticModel.hs` in review-loop Haskell artifact | pass, exit 0 | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/ghc-rerun.log` |
| 2026-06-12T23:48:15Z | `57f6306` | `grokrxiv-local-corpus-harness` | `lake env lean GrokRxiv/Proofs.lean` in review-loop Lean artifact | pass, exit 0 | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/lean-rerun.log` |
| 2026-06-12T23:48:15Z | `57f6306` | `grokrxiv-local-corpus-harness` | `grep -nE 'sorry|admit|axiom' GrokRxiv/Proofs.lean` | pass, grep exit 1 means no forbidden terms found | `agenthero/apps/grokrxiv/evals/results/20260612T232139Z/regression-pr54-weyl/lean-forbidden-grep.log` |

## Logging Rule

For corpus loop runs, write raw command output under:

```text
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/run.log
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/verdict.json
agenthero/apps/grokrxiv/evals/results/<sweep-ts>/<entry-id>/dossier.md
```

Only `LEDGER.md` is tracked by git by default; raw result directories are local evidence paths unless a human asks to commit them.
