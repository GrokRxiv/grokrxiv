import { expect, type Page, test } from "@playwright/test";

const SERVICE_TOKEN = process.env.AGENTHERO_SERVICE_TOKEN ?? "";
const SUPABASE_AUTH_URL =
  process.env.GROKRXIV_SUPABASE_AUTH_URL ?? "http://127.0.0.1:54321/auth/v1";
const MAILPIT_URL =
  process.env.GROKRXIV_MAILPIT_URL ?? "http://127.0.0.1:54324";

type AuthSettings = {
  external?: {
    email?: boolean;
    github?: boolean;
  };
};

type MailpitMessage = {
  ID: string;
  Subject: string;
  To?: Array<{ Address: string }>;
  Snippet?: string;
};

type MailpitDetail = {
  Text?: string;
  HTML?: string;
};

async function latestMagicLinkFor(email: string): Promise<string | null> {
  const listResponse = await fetch(`${MAILPIT_URL}/api/v1/messages`).catch(
    () => null,
  );
  if (!listResponse?.ok) return null;
  const list = (await listResponse.json()) as { messages?: MailpitMessage[] };
  const message = (list.messages ?? []).find((candidate) =>
    (candidate.To ?? []).some(
      (to) => to.Address.toLowerCase() === email.toLowerCase(),
    ),
  );
  if (!message) return null;
  const detailResponse = await fetch(
    `${MAILPIT_URL}/api/v1/message/${message.ID}`,
  ).catch(() => null);
  const detail = detailResponse?.ok
    ? ((await detailResponse.json()) as MailpitDetail)
    : null;
  const body = [detail?.Text, detail?.HTML, message.Snippet]
    .filter(Boolean)
    .join("\n");
  const links = Array.from(body.matchAll(/https?:\/\/[^\s"'<>]+/g)).map(
    (match) => match[0].replace(/&amp;/g, "&"),
  );
  return (
    links.find(
      (url) => url.includes("/auth/v1/verify") || url.includes("/auth/v1/callback"),
    ) ??
    links[0] ??
    null
  );
}

async function signInWithMagicLink(page: Page, email: string, next = "/dashboard") {
  await page.goto(`/login?next=${encodeURIComponent(next)}`, {
    waitUntil: "networkidle",
  });
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: /Send magic link/i }).click();
  await expect(page.getByText(/Check your email for the login link/i)).toBeVisible();

  let magicLink: string | null = null;
  await expect
    .poll(async () => {
      magicLink = await latestMagicLinkFor(email);
      return magicLink;
    })
    .not.toBeNull();
  expect(decodeURIComponent(magicLink ?? "")).toContain("/auth/callback");

  await page.goto(magicLink!, { waitUntil: "networkidle" });
  await page.waitForURL(new RegExp(next.replace("/", "\\/")));
}

test.describe("auth, pricing, quota, and visibility surfaces", () => {
  test("homepage upload is explicitly sample-only", async ({ page }) => {
    await page.goto("/", { waitUntil: "networkidle" });
    await expect(page.getByRole("heading", { name: /Try a sample review/i })).toBeVisible();
    await expect(page.getByText(/not a published GrokRxiv review/i)).toBeVisible();
    await expect(page.getByText(/Full paper reviews require an account/i)).toBeVisible();
  });

  test("login and dashboard routes are present", async ({ page }) => {
    await page.goto("/login?next=/dashboard", { waitUntil: "networkidle" });
    await expect(page.getByText("Sign in to GrokRxiv")).toBeVisible();
    await expect(page.getByRole("button", { name: /Continue with GitHub/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Send magic link/i })).toBeVisible();
    await expect(page.getByText("Failed to fetch")).toHaveCount(0);

    await page.goto("/dashboard", { waitUntil: "networkidle" });
    await expect(page).toHaveURL(/\/login\?next=(%2F|\/)dashboard/);
  });

  test("email magic link completes login and opens dashboard", async ({
    page,
    request,
  }) => {
    const settingsResponse = await request
      .get(`${SUPABASE_AUTH_URL}/settings`, { failOnStatusCode: false })
      .catch(() => null);
    test.skip(
      !settingsResponse?.ok(),
      "Supabase Auth is not reachable for this E2E run.",
    );
    const settings = (await settingsResponse!.json()) as AuthSettings;
    test.skip(
      settings.external?.email === false,
      "Email login is disabled for this E2E run.",
    );

    await signInWithMagicLink(page, `grokrxiv-e2e-${Date.now()}@example.com`);
    await expect(page.getByText("Your GrokRxiv reviews")).toBeVisible();
    await expect(page.getByText("Run a full review")).toBeVisible();
    await expect(page.getByLabel("arXiv ID")).toBeVisible();
    await expect(page.getByRole("button", { name: /Queue review/i })).toBeVisible();
    await expect(page.getByText(/Account data unavailable/i)).toHaveCount(0);
  });

  test("configured admin can open account controls", async ({ page, request }) => {
    const settingsResponse = await request
      .get(`${SUPABASE_AUTH_URL}/settings`, { failOnStatusCode: false })
      .catch(() => null);
    test.skip(
      !settingsResponse?.ok(),
      "Supabase Auth is not reachable for this E2E run.",
    );
    const settings = (await settingsResponse!.json()) as AuthSettings;
    test.skip(
      settings.external?.email === false,
      "Email login is disabled for this E2E run.",
    );

    const adminEmail =
      process.env.GROKRXIV_E2E_ADMIN_EMAIL ?? "mlong168@gmail.com";
    await signInWithMagicLink(page, adminEmail, "/admin/users");
    await expect(page.getByRole("heading", { name: /User quotas/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Update plan/i }).first()).toBeVisible();
    await expect(page.getByRole("button", { name: /Update quota/i }).first()).toBeVisible();
  });

  test("pricing page documents public, private, and no-surprise charge rules", async ({
    page,
  }) => {
    await page.goto("/pricing", { waitUntil: "networkidle" });
    await expect(
      page.getByRole("heading", {
        name: /Public reviews stay cheap/i,
      }),
    ).toBeVisible();
    await expect(page.getByText(/3 lifetime full reviews/i)).toBeVisible();
    await expect(page.getByText(/2 private reviews per month/i)).toBeVisible();
    await expect(page.getByText(/confirmed before it starts/i)).toBeVisible();
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
      "AGENTHERO_SERVICE_TOKEN is unset for this E2E run.",
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
    test.skip(
      missingProfile.status() === 503,
      "Web service token is not configured for this E2E run.",
    );
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
