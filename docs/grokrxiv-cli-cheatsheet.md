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
grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307
```

Local and git sources:

```sh
grokrxiv review --runner cli --extractor cli --type tex ./paper.tex
grokrxiv review --runner cli --extractor cli --type pdf ./paper.pdf
grokrxiv review --runner cli --extractor cli --type git <repo-url> --rev main --paper-path paper.tex
```

Already extracted:

```sh
grokrxiv extract 2605.17307 --json
grokrxiv review-extracted 2605.17307 --json
grokrxiv review-extracted --force 2605.17307 --json
```

## Batch Field Sweep

```sh
grokrxiv batch create --category math --month 2026-05 --daily-limit 30 --auto-pr --json
grokrxiv batch run <BATCH_ID> --json
grokrxiv batch status <BATCH_ID> --json
grokrxiv batch list --json
```

Run `batch run` daily from the scheduler of your choice. Batch items keep their
own state, review id, PR URL, attempts, and error fields.

## Review Ops

```sh
grokrxiv list reviews --review-status awaiting_moderation --json
grokrxiv show <REVIEW_ID> --json
grokrxiv open <REVIEW_ID>
grokrxiv request-revisions <REVIEW_ID> --notes "Needs correction."
grokrxiv approve <REVIEW_ID>
grokrxiv close <REVIEW_ID> --reason "Superseded."
grokrxiv reject <REVIEW_ID> --reason "Out of scope."
```

## Jobs

```sh
grokrxiv jobs list --kind review --state running --json
grokrxiv jobs list --state failed
```

## Runner Overrides

```sh
grokrxiv review 2605.17307 --runner cli --extractor cli --json
grokrxiv review 2605.17307 --runner api --extractor api --json
grokrxiv review 2605.17307 --runner-for technical_correctness=cli --json
grokrxiv review 2605.17307 --model-for reproducibility=gpt-5.5 --json
```

CLI mode uses the local provider CLIs and their logged-in auth state. API mode
uses provider API credentials and should be selected explicitly.

## Smoke

```sh
grokrxiv doctor --json
out="$(grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307)"
rid="$(printf '%s\n' "$out" | jq -r .review_id)"
grokrxiv show "$rid" --json
```
