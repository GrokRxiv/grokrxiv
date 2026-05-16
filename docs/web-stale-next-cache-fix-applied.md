# Web flicker/freeze — root cause was a stale `.next/` Turbopack cache (2026-05-15)

## What

Deleted `apps/web/.next/` after diagnosing that the page was not just flickering — it was in a **Turbopack panic loop**. The stale cache had `file:///workspace/...` paths baked in from an earlier Docker build, and the running native dev server couldn't write to `/workspace` (read-only / nonexistent).

## Why

After landing the pre-hydration theme-init script (`docs/web-theme-flicker-fix-applied.md`), the operator reported the page was still flickering — but actually frozen. Chrome DevTools diagnosis revealed:

1. The browser's "execution context destroyed, most likely because of a navigation" error fired immediately on every `evaluate_script` — the page was navigating away each time.
2. Network log showed only one document request at first, but the dev server log showed **repeating** Turbopack panics:

   ```
   FATAL: An unexpected Turbopack error occurred.
   failed to create directory "/workspace/apps/web/.next/dev/static/media"
   Caused by: Read-only file system (os error 30)
   ```

3. `grep -rn "/workspace" apps/web/.next` returned hundreds of hits in `.js.map` files — every chunk had `file:///workspace/apps/web/...` baked in. The original Docker build wrote these source paths; Turbopack now refused to update them.

4. With Turbopack crashing on font writes, HMR's reconnect loop was firing, and the React/Next dev runtime was retrying repeatedly. The visible symptom looked like a theme flicker but was actually full-page reloads triggered by HMR failure.

The fix is one line: `rm -rf apps/web/.next` and restart the dev server. Turbopack then rebuilds the cache from scratch with the current `/Users/mlong/Documents/Development/grokrxiv/...` paths, the font write succeeds, and the page loads cleanly.

The earlier `theme-init.js` change (FOUC prevention) is still correct — it was just masked by the panic loop, not the cause of it.

## How

```sh
pkill -f "next dev"
rm -rf apps/web/.next
cd apps/web && pnpm dev
```

After this, browser verification confirms:
- HTML has `class="dark"` immediately on first paint when `prefers-color-scheme: dark`
- Background is `rgb(2, 8, 23)` (the dark theme variable resolved correctly)
- `localStorage.theme = 'light'` + reload → no `.dark` class, white background
- Dev server log shows clean `GET / 200` with no Turbopack panic lines
- Homepage grid shows all 3 RPT1 reviews (math-ph / quant-ph / cs.AI)

## Risk

| Risk | Mitigation |
|---|---|
| Next time someone runs `pnpm dev` from a Docker container, `.next/` will get container paths baked in again | Document this gotcha in CLAUDE.md; consider adding `.next/` cleanup to a `just doctor` recipe (out of scope here) |
| Initial rebuild takes ~2.5s on first request (cold cache) vs ~80ms with warm cache | Acceptable one-time cost on every `.next` nuke |
| `tests/screenshots/` already gitignored | Verified — the screenshots from this verification land there and don't pollute the commit |

## Reversal

There is nothing to revert — the change is purely the absence of a stale directory. If someone wants to test the panic state again: switch `cd` into a Docker container with `/workspace`-mounted source, run `pnpm dev`, then `cd` back to the host and `pnpm dev` natively — that reproduces it.

## Verification

```sh
# Verify the cache is gone (or freshly rebuilt with host paths)
grep -r "/workspace" apps/web/.next 2>/dev/null | wc -l   # should be 0

# Verify Chrome sees clean theme:
# (use chrome-devtools-mcp or any browser)
# Hard reload http://localhost:3000/
# - Dark system pref → page renders dark on first paint
# - localStorage.theme=light + reload → page renders light
# Dev server log shows GET / 200 with no FATAL Turbopack lines.
```

## Follow-up

Consider adding `apps/web/.next` to a `just clean` recipe and documenting the "delete it after switching between Docker and native dev" gotcha in CLAUDE.md.
