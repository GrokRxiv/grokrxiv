# @grokrxiv/skills

Installs the review-only `grokrxiv-review` strict-JSON reviewer skill into
the Claude Code, Gemini, and Codex CLIs so that pure `--runner cli` runs of
the GrokRxiv 6-agent review DAG produce 6/6 schema-valid agent outputs.

## What it installs

| CLI | Mechanism | Destination |
|-----|-----------|-------------|
| Claude Code | `SKILL.md` with YAML frontmatter (`/skill-name` prompt prefix) | `~/.claude/skills/grokrxiv-review/` |
| Gemini | Direct copy by default. Set `GROKRXIV_SKILLS_USE_GEMINI_CLI=1` to opt-in to `gemini skills install <path> --scope user`. | `~/.gemini/skills/grokrxiv-review/` |
| Codex | Sentinel-delimited block additively merged into the global agents file | `~/.codex/AGENTS.md` |
| (all) | Canonical JSON schemas mirrored from `agenthero/apps/grokrxiv/schemas/` | `~/.grokrxiv/skills/schemas/` |

All three skill artifacts enforce the same contract for schema-bound review
roles: emit a single JSON object that strictly validates against the role's
schema. No prose, no code fences, no extra fields, no paraphrased enum
values, no stringified numbers.

The skill is not for Lean formalization, theorem statement authoring,
`Proofs.lean` generation, source-to-Lean debugging, or general coding-agent
work. Those roles should follow their own prompts and artifacts, not the
review JSON schema contract.

## Install

From the cloned repo:

```sh
node agenthero/apps/grokrxiv/skills/bin/install.js install
```

Once published (or via the github-direct invocation):

```sh
npx @grokrxiv/skills install
```

## Commands

```
grokrxiv-skills <command> [flags]

Commands:
  install         Install grokrxiv-review skill into detected CLIs.
  uninstall       Remove the skill from all locations.
  status          Report install state. Does not write anything.
  sync-schemas    Re-copy app schemas into the package.
  --help, -h      Show this help.

Flags:
  --force, -f     Overwrite existing skill directories.
  --dry-run, -n   Print actions without touching the filesystem.
```

## Behaviour

- **CLI detection** — `which claude`, `which gemini`, `which codex`. If a
  CLI is not on `PATH`, its install step is skipped (not failed); the
  process exit code is `1` instead of `0` to signal partial install.
- **Idempotent** — re-running `install` is safe. The codex block uses a
  sentinel-delimited region (`<!-- BEGIN grokrxiv-skills vX.Y.Z -->` …
  `<!-- END grokrxiv-skills vX.Y.Z -->`) that the installer strips and
  rewrites in place, never duplicating.
- **Overwrite protection** — claude / gemini skill directories that
  already exist are left untouched unless `--force` is passed.
- **Dry run** — `--dry-run` prints all planned filesystem actions without
  making them.

## Exit codes

- `0` — success, all detected CLIs installed.
- `1` — partial install (some CLI missing, or already-installed dir
  preserved without `--force`).
- `2` — fatal error.

## Tests

```sh
cd agenthero/apps/grokrxiv/skills
node --test tests/install.test.js
```

The test suite exercises:

- claude skill creation in a temp `HOME`
- codex block replacement (no duplication on reinstall)
- status reporting for a clean install
- uninstall idempotency
- `sync-schemas` copying from the repo
