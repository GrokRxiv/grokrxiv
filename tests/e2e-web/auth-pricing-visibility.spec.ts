import { expect, test } from "@playwright/test";

const SERVICE_TOKEN = process.env.GROKRXIV_SERVICE_TOKEN ?? "";

test.describe("auth, pricing, quota, and visibility surfaces", () => {
  test("homepage upload is explicitly sample-only", async ({ page }) => {
    await page.goto("/", { waitUntil: "networkidle" });
    await expect(page.getByRole("heading", { name: /Try a sample review/i })).toBeVisible();
    await expect(page.getByText(/not a published GrokRxiv review/i)).toBeVisible();
    await expect(page.getByText(/Full six-agent reviews run automatically/i)).toBeVisible();
  });

  test("login and dashboard routes are present", async ({ page }) => {
    await page.goto("/login?next=/dashboard", { waitUntil: "networkidle" });
    await expect(page.getByRole("heading", { name: /Sign in to GrokRxiv/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Continue with GitHub/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Send magic link/i })).toBeVisible();

    const dashboard = await page.request.get("/dashboard", {
      maxRedirects: 0,
      failOnStatusCode: false,
    });
    expect([307, 308]).toContain(dashboard.status());
    expect(dashboard.headers().location).toContain("/login");
  });

  test("public API only returns public visibility rows when configured", async ({
    request,
  }) => {
    const response = await request.get("/api/v1/reviews", {
      failOnStatusCode: false,
    });
    test.skip(
      response.status() === 503,
      "Supabase is not configured for this E2E run.",
    );
    expect(response.ok()).toBeTruthy();
    const body = await response.json();
    expect(Array.isArray(body.data)).toBeTruthy();
    for (const row of body.data as Array<{ visibility?: string; status?: string }>) {
      expect(row.visibility).toBe("public");
      expect(["pr_open", "published", "corrected", "rejected"]).toContain(row.status);
    }
  });

  test("API-backed review jobs require premium_api profile and cost cap", async ({
    request,
  }) => {
    test.skip(
      SERVICE_TOKEN.length === 0,
      "GROKRXIV_SERVICE_TOKEN is unset for this E2E run.",
    );

    const missingProfile = await request.post("/api/v1/review", {
      headers: { authorization: `Bearer ${SERVICE_TOKEN}` },
      data: {
        source: "2605.00561",
        type: "arxiv",
        runner: "api",
        compute_profile: "public_free",
      },
      failOnStatusCode: false,
    });
    expect(missingProfile.status()).toBe(400);
    expect(await missingProfile.json()).toHaveProperty(
      "error",
      "premium_api_requires_cost_cap",
    );

    const missingCap = await request.post("/api/v1/review", {
      headers: { authorization: `Bearer ${SERVICE_TOKEN}` },
      data: {
        source: "2605.00561",
        type: "arxiv",
        runner: "api",
        compute_profile: "premium_api",
      },
      failOnStatusCode: false,
    });
    expect(missingCap.status()).toBe(400);
    expect(await missingCap.json()).toHaveProperty(
      "error",
      "premium_api_requires_cost_cap",
    );
  });
});
