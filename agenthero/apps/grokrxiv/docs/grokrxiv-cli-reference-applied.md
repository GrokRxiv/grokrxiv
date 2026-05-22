# `agh` CLI reference

`agh` is the AgentHero operator surface for DAGOps apps, including GrokRxiv
review, batch review, moderation, publication, and diagnostics. The repo root
`.env` is loaded by the binary at startup; if it sets `AGENTHERO_ENV_FILES`,
those `.env_<purpose>` files are loaded too. Local smoke checks should use the
same CLI commands that operators run.

## Global flags

| Flag | Purpose |
|---|---|
| `--runner <api|cli|cloud|local_inference>` | Default review runner backend |
| `--extractor <api|cli>` | Extraction backend |
| `--runner-for ROLE=RUNNER` | Per-role runner override |
| `--model-for ROLE=MODEL` | Per-role model override |
| `--no-cache` | Skip review cache reads |
| `--json` | Emit machine-readable output where supported |
| `--status` / `--no-status` | Force or suppress foreground progress lines |
| `--profile <name>` | Load a named TOML runtime profile |
| `--config <path>` | Override the TOML config path |
| `--dry-run` | Print the resolved plan where supported |

## Service and diagnostics

```sh
agh serve
agh doctor
agh --json doctor
agh config
agh config --show-secrets
agh app list
agh app show grokrxiv
```

`serve` runs the HTTP API, supervisor, publish reconcile loop, and scheduler.
`doctor` is the preflight command for DB, runner, provider, and publisher
reachability.

## Review commands

```sh
agh --runner cli --extractor cli --no-cache --json app run grokrxiv review 2605.17307
agh --runner cli --extractor cli app run grokrxiv review ./paper.tex --type tex
agh --runner cli --extractor cli app run grokrxiv review ./paper.pdf --type pdf
agh --runner cli --extractor cli app run grokrxiv review <repo-url> --type git --rev main --paper-path paper.tex
```

`agh app run grokrxiv review` accepts arXiv IDs, arXiv URLs, local `.tex` and
`.pdf` files, git repositories, `@manifest` files, and stdin. For git corpus review, add
`--corpus`, `--scan-root`, `--include`, `--exclude`, and `--limit`.

Extraction-only and already-extracted paths:

```sh
agh --json app run grokrxiv extract 2605.17307
agh --json app run grokrxiv review-extracted 2605.17307
agh --json app run grokrxiv review-extracted --force 2605.17307
```

## Batch reviews

Batch review is the field-sweep surface. Full-month runs use arXiv OAI-PMH
category sets; bounded `--max-items` pilots use the human month listing order.
The command persists a batch row, schedules item rows by `daily_limit`, and
records every paper through `queued`, `running`, `reviewed`, `pr_open`,
`failed`, or `skipped`.

```sh
agh --json app run grokrxiv batch-create --category math --month 2026-05 --daily-limit 30 --auto-pr
agh --json app run grokrxiv batch-create --category math --month 2026-05 --daily-limit 4 --max-items 15 --auto-pr
agh --json app run grokrxiv batch-run <BATCH_ID>
agh --json app run grokrxiv batch-status <BATCH_ID>
agh --json app run grokrxiv batch-list
```

Use `--max-items` for bounded smoke runs before scheduling a whole month. Use
`agh app run grokrxiv batch-run` from cron, launchd, GitHub Actions, or another scheduler for a daily
review quota. With `--auto-pr`, each successfully reviewed item opens the same
GitHub review PR that `agh app run grokrxiv review` opens.

## Review lifecycle

```sh
agh --json app run grokrxiv show <REVIEW_ID>
agh app run grokrxiv open <REVIEW_ID>
```

Moderation:

```sh
agh app run grokrxiv request-revisions <REVIEW_ID> --notes "Needs a corrected proof."
agh app run grokrxiv approve <REVIEW_ID>
agh app run grokrxiv reject <REVIEW_ID> --reason "Out of scope for publication."
agh app run grokrxiv request-changes <REVIEW_ID> --notes "Regenerate after source fix."
agh app run grokrxiv close <REVIEW_ID> --reason "Superseded by a corrected review."
```

`close` hides the review from the web and closes the linked GitHub PR unless
`--keep-github-pr` is supplied.

## Jobs

```sh
agh jobs list --kind review --state running --json
agh jobs list --state failed
```

The hidden `tail-jobs` alias maps to `jobs list` for compatibility.

## Validation commands

```sh
cargo test -p agenthero-orchestrator --lib cli::tests
cargo check -p agenthero-orchestrator --all-targets
set -a && source .env && set +a
PATH="$PWD/target/release:$PATH" agh --runner cli --extractor cli --no-cache --json app run grokrxiv review 2605.17307
```
