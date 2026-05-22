import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

// Live happy-path + client-side failure tests for the upload flow.
//
// The happy-path test is gated on the orchestrator being reachable; the
// failure-path tests run with or without it (they exercise the dropzone's
// client-side validation only).
//
// Run with: pnpm --filter @grokrxiv/e2e-web test
//   or:     cd agenthero/apps/grokrxiv/tests/e2e-web && pnpm exec playwright test upload.spec.ts

const ORCHESTRATOR_URL =
  process.env.ORCHESTRATOR_URL ?? "http://localhost:8080";
const FIXTURE_PDF = path.resolve(__dirname, "../fixtures/sample.pdf");
const SCREENSHOTS_DIR = path.resolve(__dirname, "../screenshots");
fs.mkdirSync(SCREENSHOTS_DIR, { recursive: true });

const HYDRATION_RED_FLAGS = [
  "Hydration failed",
  "Text content does not match server-rendered HTML",
  "did not match",
  "There was an error while hydrating",
];

function spy(page: import("@playwright/test").Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  return errors;
}

function failOnHydrationWarning(errors: string[]) {
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

test.describe("upload flow (live)", () => {
  test("uploading sample.pdf produces Sample ready + iframe content + zip download", async ({
    page,
    request,
  }) => {
    const health = await request
      .get(`${ORCHESTRATOR_URL}/healthz`, { failOnStatusCode: false })
      .catch(() => null);
    test.skip(
      !health || !health.ok(),
      `orchestrator /healthz unreachable at ${ORCHESTRATOR_URL}; skipping happy path.`,
    );

    const errors = spy(page);
    await page.goto("/", { waitUntil: "networkidle" });
    await page.evaluate(() => {
      type CaptureWindow = Window & { __grokrxivLastBlobHeader?: number[] };
      const target = window as CaptureWindow;
      const original = URL.createObjectURL.bind(URL);
      URL.createObjectURL = (blob: Blob) => {
        void blob.arrayBuffer().then((buf) => {
          target.__grokrxivLastBlobHeader = Array.from(
            new Uint8Array(buf).slice(0, 4),
          );
        });
        return original(blob);
      };
    });

    await page
      .locator('input[type="file"]')
      .first()
      .setInputFiles(FIXTURE_PDF);

    // The success heading appears once the orchestrator returns 200.
    await expect(
      page.getByRole("heading", { name: /Sample ready/i }),
    ).toBeVisible({ timeout: 90_000 });

    // The inline iframe srcDoc rendered the review HTML.
    const frame = page.frameLocator('iframe[title="Sample review preview"]');
    await expect(frame.getByRole("heading", { name: "TL;DR" })).toBeVisible();
    await expect(
      frame.getByText(/Recommendation:\s*(Accept|Reject|Minor|Major)/),
    ).toBeVisible();

    // Download anchor is a non-empty `blob:` URL with a sensible filename.
    // The blob URL itself is proof that the dropzone successfully decoded the
    // base64 bundle and produced an in-memory zip; we don't try to fetch its
    // contents here because Playwright's evaluate context can't reach the
    // page's blob registry, and triggering a real download cancels under
    // headless mode on macOS.
    const dl = page.getByRole("link", { name: /Download sample review/i });
    await expect(dl).toBeVisible();
    const href = await dl.getAttribute("href");
    expect(href).toMatch(/^blob:http/);
    expect((href ?? "").length).toBeGreaterThan(30);
    const filename = await dl.getAttribute("download");
    expect(filename).toMatch(/grokrxiv-sample-.+\.zip$/);
    await expect
      .poll(() =>
        page.evaluate(() => {
          type CaptureWindow = Window & { __grokrxivLastBlobHeader?: number[] };
          return (window as CaptureWindow).__grokrxivLastBlobHeader ?? null;
        }),
      )
      .toEqual([0x50, 0x4b, 0x03, 0x04]);

    await page.screenshot({
      path: path.join(SCREENSHOTS_DIR, "acceptance-upload-success.png"),
      fullPage: true,
    });

    failOnHydrationWarning(errors);
  });

  test("non-PDF input is rejected client-side", async ({ page }) => {
    await page.goto("/", { waitUntil: "networkidle" });
    await page
      .locator('input[type="file"]')
      .first()
      .setInputFiles({
        name: "fake.txt",
        mimeType: "text/plain",
        buffer: Buffer.from("not a pdf"),
      });
    await expect(
      page.getByText(/Only PDF files are accepted/i),
    ).toBeVisible();
  });

  test("oversize PDF is rejected client-side", async ({ page }) => {
    await page.goto("/", { waitUntil: "networkidle" });
    // 21 MB buffer — just over the dropzone's 20 MB ceiling.
    const big = Buffer.alloc(21 * 1024 * 1024, 0x25);
    await page
      .locator('input[type="file"]')
      .first()
      .setInputFiles({
        name: "huge.pdf",
        mimeType: "application/pdf",
        buffer: big,
      });
    await expect(
      page.getByText(/File exceeds 20 MB limit/i),
    ).toBeVisible();
  });

  test("server-side PDF validation preserves orchestrator 415 status and hint", async ({
    request,
  }) => {
    const health = await request
      .get(`${ORCHESTRATOR_URL}/healthz`, { failOnStatusCode: false })
      .catch(() => null);
    test.skip(
      !health || !health.ok(),
      `orchestrator /healthz unreachable at ${ORCHESTRATOR_URL}; skipping server-side validation path.`,
    );

    const resp = await request.post("/api/upload", {
      multipart: {
        file: {
          name: "not-a-real-pdf.pdf",
          mimeType: "application/pdf",
          buffer: Buffer.from("this is not a pdf"),
        },
      },
      failOnStatusCode: false,
    });

    expect(resp.status()).toBe(415);
    const body = await resp.json();
    expect(body.upstream_status).toBe(415);
    expect(body.error).toMatch(/PDF/i);
    expect(body.hint).toBeTruthy();
  });
});
