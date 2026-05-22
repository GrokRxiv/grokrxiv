# `agh` CLI cheatsheet

## Setup

```sh
cp .env.example .env
for name in core ingest extract review publish web billing dev; do
  cp ".env_${name}.example" ".env_${name}"
done
supabase start
cargo build --workspace
agh doctor
agh app list
```

The binary loads root `.env` from the repo root and then each file named by
`AGENTHERO_ENV_FILES`. For shell scripts that run the release binary directly,
source the root file plus its includes:

```sh
set -a
source .env
for file in ${AGENTHERO_ENV_FILES//,/ }; do source "$file"; done
set +a
PATH="$PWD/target/release:$PATH"
```

## One Paper

```sh
agh --runner cli --extractor cli --no-cache --json app run grokrxiv -- review 2605.17307
```

Local and git sources:

```sh
agh --runner cli --extractor cli app run grokrxiv -- review ./paper.tex --type tex
agh --runner cli --extractor cli app run grokrxiv -- review ./paper.pdf --type pdf
agh --runner cli --extractor cli app run grokrxiv -- review <repo-url> --type git --rev main --paper-path paper.tex
```

Already extracted:

```sh
agh --json app run grokrxiv -- extract 2605.17307
agh --json app run grokrxiv -- review-extracted 2605.17307
agh --json app run grokrxiv -- review-extracted --force 2605.17307
```

## Batch Field Sweep

```sh
agh --json app run grokrxiv -- batch-create --category math --month 2026-05 --daily-limit 30 --auto-pr
agh --json app run grokrxiv -- batch-run <BATCH_ID>
agh --json app run grokrxiv -- batch-status <BATCH_ID>
agh --json app run grokrxiv -- batch-list
```

Run `batch run` daily from the scheduler of your choice. Batch items keep their
own state, review id, PR URL, attempts, and error fields.

## Review Ops

```sh
agh --json app run grokrxiv -- show <REVIEW_ID>
agh app run grokrxiv -- open <REVIEW_ID>
agh app run grokrxiv -- request-revisions <REVIEW_ID> --notes "Needs correction."
agh app run grokrxiv -- approve <REVIEW_ID>
agh app run grokrxiv -- close <REVIEW_ID> --reason "Superseded."
agh app run grokrxiv -- reject <REVIEW_ID> --reason "Out of scope."
```

## Jobs

```sh
agh jobs list --kind review --state running --json
agh jobs list --state failed
```

## Runner Overrides

```sh
agh --runner cli --extractor cli --json app run grokrxiv -- review 2605.17307
agh --runner api --extractor api --json app run grokrxiv -- review 2605.17307
agh --runner-for technical_correctness=cli --json app run grokrxiv -- review 2605.17307
agenthero --model-for reproducibility=gpt-5.5 --json app run grokrxiv -- review 2605.17307
```

CLI mode uses the local provider CLIs and their logged-in auth state. API mode
uses provider API credentials and should be selected explicitly.

## Smoke

```sh
agh --json doctor
out="$(agh --runner cli --extractor cli --no-cache --json app run grokrxiv -- review 2605.17307)"
rid="$(printf '%s\n' "$out" | jq -r .review_id)"
agh --json app run grokrxiv -- show "$rid"
```
