import { defineConfig, devices } from "@playwright/test";
import { createHash } from "node:crypto";

const worktreePort =
  5200 +
  (parseInt(createHash("sha256").update(process.cwd()).digest("hex").slice(0, 4), 16) %
    1000);
const port = Number(
  process.env.MESH_TALK_E2E_PORT ?? (process.env.CI ? 5173 : worktreePort),
);
const url = `http://localhost:${port}`;

// Browser E2E for the Mesh-Talk UI. Runs the Vite dev server + drives the real React app in
// headless Chromium with a mocked Tauri IPC layer (see e2e/tauri-mock.ts).
export default defineConfig({
  testDir: "./e2e",
  // A cold Vite transform on Windows can consume most of Playwright's 30s
  // default before the first page becomes interactive.
  timeout: process.platform === "win32" ? 90_000 : 30_000,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  // A single macOS worker avoids Vite asset decode retries and page-load
  // timeouts seen in the full suite with four concurrent browsers. CI already
  // uses one; Windows keeps its existing four-worker limit.
  workers:
    process.env.CI || process.platform === "darwin"
      ? 1
      : process.platform === "win32"
        ? 4
        : undefined,
  reporter: process.env.CI ? "list" : "html",
  snapshotPathTemplate: "e2e/{testFileName}-snapshots/{arg}{ext}",
  expect: {
    toHaveScreenshot: {
      maxDiffPixelRatio: 0.015,
    },
  },
  use: {
    baseURL: url,
    timezoneId: "Asia/Tokyo",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `npm run dev -- --port ${port}`,
    env: { MESH_TALK_E2E: "1" },
    url,
    // Another worktree may already have Vite on this port. Reusing it can
    // silently test an older checkout and report a false pass or failure.
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
