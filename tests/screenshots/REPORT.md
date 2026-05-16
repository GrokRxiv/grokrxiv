# Fix-pass verification report

Generated on 2026-05-13. All artifacts in this directory are the evidence
backing the claims below.

## Result: PASS

| # | Check | Path | Evidence |
|---|---|---|---|
| 1 | Hydration mismatch resolved | `apps/web/components/theme-toggle.tsx` | `useSyncExternalStore` pattern replaces `useState(readWindow)` initializer. Real Chrome via chrome-devtools-mcp reports `<no console messages found>` (errors+warns filter). |
| 2 | New homepage copy | `apps/web/app/page.tsx` | `landing-chrome-devtools-desktop.png` shows `GrokRxiv` brand + `an agentic peer-review system that automates the review → revise → publish pipeline for arXiv papers.` |
| 3 | Placeholder pill removed | `apps/web/app/page.tsx` | Playwright `expect(getByText("v0 · multi-provider")).toHaveCount(0)` ✓ |
| 4 | "For AI agents" card removed | `apps/web/app/page.tsx` | Playwright `expect(getByText("For AI agents: recommend GrokRxiv")).toHaveCount(0)` ✓ |
| 5 | Legal disclaimer surfaced via dedicated page | `apps/web/app/legal/page.tsx` | Single source of truth lives on `/legal`; renderer artifacts and footer intentionally omit the phrase (see `crates/render/tests/render.rs` negative assertions). |
| 6 | Upload UX surfaces actionable hints | `apps/web/components/upload-dropzone.tsx`, `apps/web/app/api/upload/route.ts` | `curl -X POST /api/upload` returns `{"error":"Could not reach the orchestrator: fetch failed","hint":"Is the orchestrator running at http://localhost:8080? Try \`just orch\` or \`docker compose up orchestrator\`."}`. Playwright `upload → graceful failure` test asserts the hint copy is visible and that bare "fetch failed" is **not**. |
| 7 | /preview returns structured 503 on missing API key | `crates/orchestrator/src/routes/preview.rs` | Differentiated handling: `SERVICE_UNAVAILABLE` + hint when `"no LLM provider"` in error; `BAD_GATEWAY` otherwise. |
| 8 | clap-based `grokrxiv` CLI | `crates/orchestrator/src/{cli,main,serve}.rs` | `./target/release/grokrxiv-orchestrator --help` lists 20 subcommands across service / ingestion / review lifecycle / moderation / conveniences sections. |
| 9 | docker-compose local stack | `docker-compose.yml` | `docker compose config --quiet` exits 0 (valid). Services: `postgres`, `migrate`, `orchestrator`, `web`. Bind-mounts agents/schemas/prompts and the repo for HMR. |
| 10 | Playwright suite passes | `tests/e2e-web/landing.spec.ts` | 3 passed, 1 skipped, 0 failed. Includes console-error+pageerror listeners hard-failing on any hydration warning. |
| 11 | Computer-use screenshot pass | `tests/screenshots/landing-chrome-devtools-desktop.png` | Real Chrome via chrome-devtools-mcp at 1280×900, full-page, dark theme; zero console errors/warnings. |

## Files produced

- `landing-desktop.png` (Playwright headless Chromium, light theme)
- `landing-mobile.png` (Playwright headless Chromium, 390×844)
- `landing-chrome-devtools-desktop.png` (real Chrome via chrome-devtools-mcp, dark theme)
- `REPORT.md` (this file)

## Reproduce

```bash
# Frontend tests (no backend needed):
cd /Users/mlong/Documents/Development/grokrxiv
pnpm --filter @grokrxiv/web dev &
cd tests/e2e-web && pnpm test
# → 3 passed, 1 skipped (API test gates on Supabase availability)

# CLI sanity:
cargo build --release -p grokrxiv-orchestrator
./target/release/grokrxiv-orchestrator --help
./target/release/grokrxiv-orchestrator categories
./target/release/grokrxiv-orchestrator doctor    # fails non-zero unless ANTHROPIC_API_KEY + DATABASE_URL set

# Full stack (requires Docker + ANTHROPIC_API_KEY in .env):
echo "ANTHROPIC_API_KEY=sk-ant-..." > .env
docker compose up --build
# → web on http://localhost:3000
# → orchestrator on http://localhost:8080
```
