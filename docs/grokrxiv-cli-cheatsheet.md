# `grokrxiv` CLI cheatsheet

## Setup

```sh
cp .env.example .env
for name in core ingest extract review publish web billing dev; do
  cp ".env_${name}.example" ".env_${name}"
done
supabase start
cargo build --workspace
grokrxiv doctor
grokrxiv app list
```

The binary loads root `.env` from the repo root and then each file named by
`GROKRXIV_ENV_FILES`. For shell scripts that run the release binary directly,
source the root file plus its includes:

```sh
set -a
source .env
for file in ${GROKRXIV_ENV_FILES//,/ }; do source "$file"; done
set +a
PATH="$PWD/target/release:$PATH"
```

## One Paper

```sh
grokrxiv --runner cli --extractor cli --no-cache --json app run research review 2605.17307
```

Local and git sources:

```sh
grokrxiv --runner cli --extractor cli app run research review ./paper.tex --type tex
grokrxiv --runner cli --extractor cli app run research review ./paper.pdf --type pdf
grokrxiv --runner cli --extractor cli app run research review <repo-url> --type git --rev main --paper-path paper.tex
```

Already extracted:

```sh
grokrxiv --json app run research extract 2605.17307
grokrxiv --json app run research review-extracted 2605.17307
grokrxiv --json app run research review-extracted --force 2605.17307
```

## Batch Field Sweep

```sh
grokrxiv --json app run research batch-create --category math --month 2026-05 --daily-limit 30 --auto-pr
grokrxiv --json app run research batch-run <BATCH_ID>
grokrxiv --json app run research batch-status <BATCH_ID>
grokrxiv --json app run research batch-list
```

Run `batch run` daily from the scheduler of your choice. Batch items keep their
own state, review id, PR URL, attempts, and error fields.

## Review Ops

```sh
grokrxiv --json app run research show <REVIEW_ID>
grokrxiv app run research open <REVIEW_ID>
grokrxiv app run research request-revisions <REVIEW_ID> --notes "Needs correction."
grokrxiv app run research approve <REVIEW_ID>
grokrxiv app run research close <REVIEW_ID> --reason "Superseded."
grokrxiv app run research reject <REVIEW_ID> --reason "Out of scope."
```

## Jobs

```sh
grokrxiv jobs list --kind review --state running --json
grokrxiv jobs list --state failed
```

## Runner Overrides

```sh
grokrxiv --runner cli --extractor cli --json app run research review 2605.17307
grokrxiv --runner api --extractor api --json app run research review 2605.17307
grokrxiv --runner-for technical_correctness=cli --json app run research review 2605.17307
grokrxiv --model-for reproducibility=gpt-5.5 --json app run research review 2605.17307
```

CLI mode uses the local provider CLIs and their logged-in auth state. API mode
uses provider API credentials and should be selected explicitly.

## Smoke

```sh
grokrxiv doctor --json
out="$(grokrxiv --runner cli --extractor cli --no-cache --json app run research review 2605.17307)"
rid="$(printf '%s\n' "$out" | jq -r .review_id)"
grokrxiv --json app run research show "$rid"
```
