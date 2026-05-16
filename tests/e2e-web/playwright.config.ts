import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.GROKRXIV_BASE_URL ?? "http://localhost:3000";

export default defineConfig({
  testDir: "./",
  timeout: 90_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL,
    trace: "on-first-retry",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 800 } },
    },
  ],
});
