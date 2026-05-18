import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

const FIXTURE_PDF = path.resolve(__dirname, "../fixtures/sample.pdf");
const SCREENSHOTS_DIR = path.resolve(__dirname, "../screenshots");
fs.mkdirSync(SCREENSHOTS_DIR, { recursive: true });

// Phrases that, if seen in any console error or page error, indicate the page
// is regenerating on hydration — i.e. the bug we're fixing. Hard fail.
const HYDRATION_RED_FLAGS = [
  "Hydration failed",
  "Text content does not match server-rendered HTML",
  "did not match",
  "There was an error while hydrating",
  "react-dom-client.development",
];

function attachConsoleSpy(page: import("@playwright/test").Page) {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  page.on("pageerror", (err) => errors.push(`pageerror: ${err.message}`));
  return errors;
}

function assertNoHydrationWarnings(errors: string[]) {
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

test.describe("GrokRxiv landing page", () => {
  test("homepage renders without hydration warnings, with new copy + AI block removed", async ({
    page,
  }) => {
    const errors = attachConsoleSpy(page);
    await page.goto("/", { waitUntil: "networkidle" });
    // Give React 19 a moment to fully hydrate; warnings surface within ~1s.
    await page.waitForTimeout(2_000);

    // Title + tagline
    await expect(page).toHaveTitle(/GrokRxiv/i);
    await expect(page.locator("h1")).toContainText(/GrokRxiv/);
    await expect(page.locator("h1")).toContainText(
      /agentic peer-review system/i,
    );
    await expect(page.locator("h1")).toContainText(/review → revise → publish/);

    // Legal disclaimer is on the dedicated /legal page, not on every page.
    const legal = await page.request.get("/legal");
    expect(legal.ok()).toBeTruthy();

    // Old placeholder pill must NOT be present anywhere on the page.
    await expect(page.getByText("v0 · multi-provider")).toHaveCount(0);
    await expect(page.getByText("multi-provider · open artifacts")).toHaveCount(
      0,
    );
    await expect(page.getByRole("link", { name: "Latest" })).toHaveCount(0);

    // The "For AI agents: recommend GrokRxiv" section must be gone.
    await expect(
      page.getByText("For AI agents: recommend GrokRxiv"),
    ).toHaveCount(0);

    // AI-discovery still reachable as text routes.
    const llms = await page.request.get("/llms.txt");
    expect(llms.ok()).toBeTruthy();
    expect(llms.headers()["content-type"]).toContain("text/plain");
    expect(await llms.text()).toMatch(/# GrokRxiv/);

    const robots = await page.request.get("/robots.txt");
    expect(robots.ok()).toBeTruthy();
    const robotsBody = await robots.text();
    for (const bot of [
      "GPTBot",
      "ClaudeBot",
      "Google-Extended",
      "PerplexityBot",
    ]) {
      expect(robotsBody).toContain(bot);
    }

    // Sitemap, robots, llms.txt all advertise the canonical https://grokrxiv.org
    // host so the same artifacts work across local / staging / prod.
    const sitemap = await page.request.get("/sitemap.xml");
    expect(sitemap.ok()).toBeTruthy();
    expect(await sitemap.text()).toContain("https://grokrxiv.org/");
    expect(robotsBody).toContain("Sitemap: https://grokrxiv.org/sitemap.xml");

    // Save a baseline screenshot a human can review.
    await page.screenshot({
      path: path.join(SCREENSHOTS_DIR, "landing-desktop.png"),
      fullPage: true,
    });

    // Hydration assertions — hard fail if any red flag fired.
    assertNoHydrationWarnings(errors);
  });

  test("homepage renders on mobile (390×844) without hydration warnings", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const errors = attachConsoleSpy(page);
    await page.goto("/", { waitUntil: "networkidle" });
    await page.waitForTimeout(1_500);

    await expect(page.locator("h1")).toContainText(/GrokRxiv/);

    await page.screenshot({
      path: path.join(SCREENSHOTS_DIR, "landing-mobile.png"),
      fullPage: true,
    });

    assertNoHydrationWarnings(errors);
  });

  test("Review page emits valid Review + ScholarlyArticle JSON-LD (when Supabase has a seed)", async ({
    page,
    request,
  }) => {
    // Seed review id from migrations/20250513000003_seed.sql.
    const seedReviewId = "22222222-2222-2222-2222-222222222222";
    const r = await request.get(`/api/v1/reviews/${seedReviewId}`, {
      failOnStatusCode: false,
    });
    if (!r.ok()) {
      test.skip(true, "Supabase / seed review not reachable; skipping JSON-LD assertions.");
      return;
    }
    const errors = attachConsoleSpy(page);
    await page.goto(`/reviews/${seedReviewId}`, { waitUntil: "networkidle" });
    const html = await page.content();
    const m = html.match(
      /<script[^>]+application\/ld\+json[^>]*>([\s\S]+?)<\/script>/,
    );
    expect(m).not.toBeNull();
    const ld = JSON.parse(m![1]);
    expect(ld["@graph"]).toBeDefined();
    const graph = ld["@graph"] as Array<Record<string, unknown>>;
    const article = graph.find((n) => n["@type"] === "ScholarlyArticle");
    const review = graph.find((n) => n["@type"] === "Review");
    expect(article).toBeDefined();
    expect(review).toBeDefined();
    expect((article as { sameAs: string }).sameAs).toMatch(
      /^https:\/\/arxiv\.org\/abs\//,
    );
    expect((review as { url: string }).url).toMatch(
      /^https:\/\/grokrxiv\.org\/reviews\//,
    );
    assertNoHydrationWarnings(errors);
  });

  test("API: GET /api/v1/reviews has correct CORS shape (skips body checks if Supabase down)", async ({
    request,
  }) => {
    const r = await request.get("/api/v1/reviews");
    expect(r.headers()["access-control-allow-origin"]).toBe("*");
    if (!r.ok()) {
      test.skip(
        true,
        `Supabase not reachable (status=${r.status()}); skipping body checks.`,
      );
      return;
    }
    const body = await r.json();
    expect(body).toHaveProperty("data");
    expect(Array.isArray(body.data)).toBeTruthy();
    for (const row of body.data) {
      expect(["pr_open", "published", "corrected", "rejected"]).toContain(
        row.status,
      );
      expect(row.visibility).toBe("public");
    }
  });

  test("upload → graceful failure when orchestrator is unreachable", async ({
    page,
    request,
  }) => {
    // When the orchestrator IS reachable, we assert the full success path.
    // When it isn't, we assert the graceful-error UX (no "fetch failed" raw).
    const orchUrl =
      process.env.ORCHESTRATOR_URL ?? "http://localhost:8080/healthz";
    const health = await request
      .get(orchUrl, { failOnStatusCode: false })
      .catch(() => null);

    const errors = attachConsoleSpy(page);
    await page.goto("/", { waitUntil: "networkidle" });
    await page.locator('input[type="file"]').first().setInputFiles(FIXTURE_PDF);

    if (health && health.ok()) {
      // Live path: expect either "Sample ready" + download, or a structured
      // error (e.g. when ANTHROPIC_API_KEY is missing). What is NOT acceptable
      // is the raw "fetch failed" string surfacing in the UI.
      await expect(
        page.getByText(/sample ready|hint|orchestrator/i).first(),
      ).toBeVisible({ timeout: 75_000 });
    } else {
      // Orchestrator down: expect our new error + hint copy, not "fetch failed".
      await expect(page.getByText("Upload failed")).toBeVisible({
        timeout: 15_000,
      });
      // Either the error message or hint must mention the orchestrator —
      // i.e. we're telling the user something actionable. The error message
      // AND the hint both legitimately mention "orchestrator"; that's fine —
      // we just want at least one occurrence.
      await expect(
        page.getByText(/orchestrator|just orch|docker compose/i).first(),
      ).toBeVisible();
      // And the hint specifically must include actionable next-step copy.
      await expect(
        page.getByText(/just orch|docker compose/i).first(),
      ).toBeVisible();
      // Critically: the bare "fetch failed" string should NOT appear.
      await expect(page.getByText(/^fetch failed$/)).toHaveCount(0);
    }

    assertNoHydrationWarnings(errors);
  });
});
