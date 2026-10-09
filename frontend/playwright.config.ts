import { defineConfig, devices } from "@playwright/test";

// Browser E2E for the Mesh-Talk UI. Runs the Vite dev server + drives the real React app in
// headless Chromium with a mocked Tauri IPC layer (see e2e/tauri-mock.ts).
export default defineConfig({
  testDir: "./e2e",
  // A cold Vite transform on Windows can consume most of Playwright's 30s
  // default before the first page becomes interactive.
  timeout: process.platform === "win32" ? 60_000 : 30_000,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  // Eight concurrent WebViews overwhelm the Windows test host and make visual
  // snapshots unstable; four keep the suite responsive during full E2E runs.
  workers: process.env.CI ? 1 : process.platform === "win32" ? 4 : undefined,
  reporter: process.env.CI ? "list" : "html",
  snapshotPathTemplate: "e2e/{testFileName}-snapshots/{arg}{ext}",
  expect: {
    toHaveScreenshot: {
      maxDiffPixelRatio: 0.015,
    },
  },
  use: {
    baseURL: "http://localhost:5173",
    timezoneId: "Asia/Tokyo",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
