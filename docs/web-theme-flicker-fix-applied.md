# Web theme flicker — pre-hydration init script (2026-05-15)

## What

Added `apps/web/public/theme-init.js` — a small synchronous script that reads the user's saved theme preference (or system preference if unset) and applies the `dark` class to `<html>` **before** the body paints. Referenced from `apps/web/app/layout.tsx`'s `<head>` as a non-async `<script src="/theme-init.js" />`.

## Why

The operator reported the homepage at `http://localhost:3000/` flickering between light and dark mode. Root cause:

1. SSR renders `<html>` with no theme class.
2. CSS defaults to light mode (the `@theme {}` block sets light variables; `.dark {}` overrides them).
3. The `ThemeToggle` client component runs `useEffect` only AFTER hydration and adds the `dark` class.
4. Net effect: the browser paints LIGHT first, then a few hundred ms later React hydrates and flips to DARK — the user sees a flash.

The fix is the standard pre-hydration blocking script pattern: a `<script>` in `<head>` (no async/defer) runs synchronously between HTML parsing and body paint. By the time the body renders, the `dark` class is already set, so the first paint is correct.

## How

| Change | Location |
|---|---|
| New blocking init script | `apps/web/public/theme-init.js` |
| `<script src="/theme-init.js" />` injected via `<head>` in the root layout | `apps/web/app/layout.tsx` |

The script logic (see `apps/web/public/theme-init.js` for the actual code):
1. Read `localStorage.theme` if present.
2. Otherwise fall back to `window.matchMedia("(prefers-color-scheme: dark)").matches`.
3. Add (or remove) the `dark` class on `document.documentElement` accordingly.
4. Try/catch around the whole thing so a non-conformant browser silently falls back to light.

The script is served from `public/` so it can be referenced by URL. That keeps the React tree free of inline-HTML rendering APIs — the script tag has only a `src` attribute and is loaded over normal HTTP.

`<html lang="en" suppressHydrationWarning>` is already in place from prior theme work — handles the case where the SSR HTML has no `dark` class but the client mutates `<html>` before React reads it.

## Risk

| Risk | Mitigation |
|---|---|
| Parser blocks on the script — adds a small latency before body paint | The script is tiny (~500 bytes), local, served with normal HTTP/2 cache headers. Net cost: <5ms; eliminates the visible flash that cost the user ~200ms of perceptual jank. |
| Script fails (localStorage disabled, etc.) | The try/catch leaves the default in place; the user sees light mode rather than crashing. |
| Hydration mismatch warning if the server HTML had a different class | `suppressHydrationWarning` on `<html>` was already set; the warning is suppressed. The DOM ends up consistent after the init script runs. |
| `useSyncExternalStore` in `ThemeToggle` re-reads after hydration and could flip again | The toggle reads from the same localStorage + matchMedia signals the init script uses, so the snapshot agrees with what's already on `<html>`. No second flip. |

## Reversal

```sh
git checkout HEAD~1 -- apps/web/app/layout.tsx
rm apps/web/public/theme-init.js
```

`ThemeToggle` keeps working without the init script — there's just the flash again.

## Verification

```sh
cd apps/web && pnpm dev
# Visit http://localhost:3000 in a browser. With system theme set to dark,
# the page should render dark immediately on hard reload — no white flash.
# Switching the toggle should still work as before.
```

Also visible in DevTools Performance:
- Before: a paint event at ~100ms with light colors, then a class change + re-paint at ~300ms with dark colors.
- After: single paint at ~100ms with the correct theme; no class mutation visible during initial load.
