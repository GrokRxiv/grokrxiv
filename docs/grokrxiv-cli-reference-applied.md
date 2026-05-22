# `grokrxiv` CLI reference

The `grokrxiv` binary is the operator surface for local review, batch review,
moderation, publication, and diagnostics. The repo root `.env` is loaded by the
binary at startup; if it sets `GROKRXIV_ENV_FILES`, those `.env_<purpose>` files
are loaded too. Local smoke checks should use the same CLI commands that
operators run.

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
grokrxiv serve
grokrxiv doctor
grokrxiv doctor --json
grokrxiv config
grokrxiv config --show-secrets
grokrxiv app list
grokrxiv app show research
```

`serve` runs the HTTP API, supervisor, publish reconcile loop, and scheduler.
`doctor` is the preflight command for DB, runner, provider, and publisher
reachability.

## Review commands

```sh
grokrxiv --runner cli --extractor cli --no-cache --json app run research review 2605.17307
grokrxiv --runner cli --extractor cli app run research review ./paper.tex --type tex
grokrxiv --runner cli --extractor cli app run research review ./paper.pdf --type pdf
grokrxiv --runner cli --extractor cli app run research review <repo-url> --type git --rev main --paper-path paper.tex
```

`app run research review` accepts arXiv IDs, arXiv URLs, local `.tex` and
`.pdf` files, git repositories, `@manifest` files, and stdin. For git corpus review, add
`--corpus`, `--scan-root`, `--include`, `--exclude`, and `--limit`.

Extraction-only and already-extracted paths:

```sh
grokrxiv --json app run research extract 2605.17307
grokrxiv --json app run research review-extracted 2605.17307
grokrxiv --json app run research review-extracted --force 2605.17307
```

## Batch reviews

Batch review is the field-sweep surface. Full-month runs use arXiv OAI-PMH
category sets; bounded `--max-items` pilots use the human month listing order.
The command persists a batch row, schedules item rows by `daily_limit`, and
records every paper through `queued`, `running`, `reviewed`, `pr_open`,
`failed`, or `skipped`.

```sh
grokrxiv --json app run research batch-create --category math --month 2026-05 --daily-limit 30 --auto-pr
grokrxiv --json app run research batch-create --category math --month 2026-05 --daily-limit 4 --max-items 15 --auto-pr
grokrxiv --json app run research batch-run <BATCH_ID>
grokrxiv --json app run research batch-status <BATCH_ID>
grokrxiv --json app run research batch-list
```

Use `--max-items` for bounded smoke runs before scheduling a whole month. Use
`app run research batch-run` from cron, launchd, GitHub Actions, or another scheduler for a daily
review quota. With `--auto-pr`, each successfully reviewed item opens the same
GitHub review PR that `grokrxiv app run research review` opens.

## Review lifecycle

```sh
grokrxiv --json app run research show <REVIEW_ID>
grokrxiv app run research open <REVIEW_ID>
```

Moderation:

```sh
grokrxiv app run research request-revisions <REVIEW_ID> --notes "Needs a corrected proof."
grokrxiv app run research approve <REVIEW_ID>
grokrxiv app run research reject <REVIEW_ID> --reason "Out of scope for publication."
grokrxiv app run research request-changes <REVIEW_ID> --notes "Regenerate after source fix."
grokrxiv app run research close <REVIEW_ID> --reason "Superseded by a corrected review."
```

`close` hides the review from the web and closes the linked GitHub PR unless
`--keep-github-pr` is supplied.

## Jobs

```sh
grokrxiv jobs list --kind review --state running --json
grokrxiv jobs list --state failed
```

The hidden `tail-jobs` alias maps to `jobs list` for compatibility.

## Validation commands

```sh
cargo test -p grokrxiv-orchestrator --lib cli::tests
cargo check -p grokrxiv-orchestrator --all-targets
set -a && source .env && set +a
PATH="$PWD/target/release:$PATH" grokrxiv --runner cli --extractor cli --no-cache --json app run research review 2605.17307
```
