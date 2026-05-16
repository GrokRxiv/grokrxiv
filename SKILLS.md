# Skills index

Skills live under `.claude/skills/<skill-name>/SKILL.md`. Each skill is a short instruction file Claude reads on demand to do a specific job.

| Skill | Status | Purpose |
|---|---|---|
| `publish-plan` | FP6 (this pass) | Convert a markdown plan or research doc → self-contained HTML in `research/`, and surface it in the local research viewer. Used by the Stop-hook to keep `research/*.md` and `research/*.html` in sync. |
| `grokrxiv-review` | FP6+ / Tier-2 enforcement | (FP5 design) Enforce JSON-schema-valid output when invoking Claude Code or Codex CLI as a subscription-tier reviewer. Inlines the 6 role schemas; output validated with a 2× retry envelope. **Not implemented yet** — planned for FP6b/FP6e once the Tier-2 microVMs come online. |

## Invocation

These skills are invoked via the `Skill` tool. They are not slash commands. The agent decides when to invoke based on the description.

## Hooks that use skills

- **Stop hook** (`.claude/settings.json` → `.claude/hooks/maybe-publish-research.sh`) — after each agent turn, scans for stale `research/*.md` and invokes the build pipeline behind `publish-plan`.

## Adding a new skill

1. Create `.claude/skills/<name>/SKILL.md` with `name:` and `description:` frontmatter.
2. Add a row to the table above.
3. If the skill should be triggered automatically, register it in `.claude/settings.json` under the appropriate hook.
