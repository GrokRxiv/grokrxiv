import { expect, test, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

// Cross-route × cross-viewport responsiveness suite.
//
// For every (route, viewport) we:
//   1. set the viewport
//   2. navigate
//   3. spy on console.error + pageerror, hard-fail on any hydration warning
//   4. assert no horizontal overflow at document.body level
//   5. screenshot the result for a human to skim

const SCREENSHOTS_DIR = path.resolve(__dirname, "../screenshots");
fs.mkdirSync(SCREENSHOTS_DIR, { recursive: true });

const ROUTES: ReadonlyArray<string> = [
  "/",
  "/about",
  "/api-docs",
  "/legal",
  "/reviews/22222222-2222-2222-2222-222222222222",
  "/papers/2401.12345",
];

const VIEWPORTS: ReadonlyArray<{
  name: "mobile" | "tablet" | "desktop";
  width: number;
  height: number;
}> = [
  { name: "mobile", width: 375, height: 667 },
  { name: "tablet", width: 768, height: 1024 },
  { name: "desktop", width: 1280, height: 800 },
];

const HYDRATION_RED_FLAGS = [
  "Hydration failed",
  "Text content does not match server-rendered HTML",
  "did not match",
  "There was an error while hydrating",
];

function attachConsoleSpy(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  page.on("pageerror", (err) => errors.push(`pageerror: ${err.message}`));
  return errors;
}

function failOnHydrationWarning(errors: string[]): void {
  const offending = errors.filter((e) =>
    HYDRATION_RED_FLAGS.some((flag) => e.includes(flag)),
  );
  if (offending.length > 0) {
    throw new Error(
      "Hydration mismatch detected in browser console:\n  " +
        offending.join("\n  "),
    );
  }
}

function routeSlug(route: string): string {
  if (route === "/") return "home";
  return route
    .replace(/^\/+/, "")
    .replace(/\/+$/, "")
    .replace(/[^a-zA-Z0-9-]+/g, "-");
}

test.describe("Responsive layout × viewport matrix", () => {
  for (const route of ROUTES) {
    for (const viewport of VIEWPORTS) {
      test(`${route} @ ${viewport.name} (${viewport.width}×${viewport.height}) — no overflow, no hydration warning`, async ({
        page,
      }) => {
        await page.setViewportSize({
          width: viewport.width,
          height: viewport.height,
        });
        const errors = attachConsoleSpy(page);
        await page.goto(route, { waitUntil: "networkidle" });
        // React 19 hydration warnings surface within ~1s.
        await page.waitForTimeout(1_500);

        // Assert there is no horizontal overflow at the document level.
        // Pass innerWidth in via the evaluate so the comparison is single-trip.
        const overflow = await page.evaluate(() => {
          return {
            scrollWidth: document.body.scrollWidth,
            innerWidth: window.innerWidth,
          };
        });
        // Allow a 1-px rounding tolerance for sub-pixel layout.
        expect(
          overflow.scrollWidth,
          `Horizontal overflow on ${route} @ ${viewport.name}: ` +
            `body.scrollWidth=${overflow.scrollWidth} > innerWidth=${overflow.innerWidth}`,
        ).toBeLessThanOrEqual(overflow.innerWidth + 1);

        await page.screenshot({
          path: path.join(
            SCREENSHOTS_DIR,
            `responsive-${routeSlug(route)}-${viewport.name}.png`,
          ),
          fullPage: true,
        });

        failOnHydrationWarning(errors);
      });
    }
  }
});
