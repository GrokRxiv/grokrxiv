---
name: publish-plan
description: Convert a markdown plan or research doc into a self-contained HTML artifact in research/, then surface it in the local research site index. Use after editing any research/*.md or after a plan ships from ~/.claude/plans/.
---

# publish-plan

Use this skill to convert a markdown document into a styled, self-contained HTML artifact viewable in any browser and in the local research site at research/site/.

## When to invoke

- Just edited or wrote a `research/*.md` doc
- Just marked a plan Shipped in `~/.claude/plans/piped-bubbling-brook.md` and want the corresponding `fpN-*.md` artifact published as HTML
- A user explicitly asks for a research doc to be (re)generated

## How to invoke

Given a markdown file at PATH:

1. Read the markdown file.
2. Determine the output path:
   - If PATH is under `research/` → output to `research/{basename}.html`
   - If PATH is under `~/.claude/plans/` → output to `research/{basename}.html` (so all plans appear in the same viewer)
3. Run the build script:
   ```sh
   node research/_template/build.mjs "<PATH>"
   ```
   The script handles path normalization internally; just pass the absolute path.
4. Confirm the output file exists at the expected location. Echo the path back.
5. Touch `research/site/lib/.search-index-dirty` (create the file if needed) so the Next.js viewer regenerates its search index on next dev reload:
   ```sh
   mkdir -p research/site/lib
   date -u +%s > research/site/lib/.search-index-dirty
   ```

## Failure handling

If the build script exits non-zero, surface the stderr output. Common causes:
- `research/_template/node_modules` not installed — instruct the user to run `cd research/_template && pnpm install`
- Markdown syntax error — surface the line number from the build script's error
- KaTeX math syntax error — fix the math expression in the source `.md`

Do NOT modify the markdown source to "fix" rendering issues. Always trace back to the source error.
