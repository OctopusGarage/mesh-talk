import { defineConfig } from "@playwright/test";

/** Public Pages tests host their own static fixtures and generated readers.
 * No Vite server, desktop mocks or Rust build is needed for this boundary. */
export default defineConfig({
  testDir: "./e2e",
  testMatch: [
    "site-audit.spec.ts",
    "site-discovery.spec.ts",
    "site-motion.spec.ts",
    "site-faq-motion.spec.ts",
    "site-network-flow.spec.ts",
    "site-language.spec.ts",
    "site-downloads.spec.ts",
    "help-website.spec.ts",
  ],
  timeout: 30_000,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: process.env.CI ? 1 : 3,
  reporter: "list",
  outputDir: "test-results/public-site",
  use: { trace: "retain-on-failure" },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
    { name: "firefox", use: { browserName: "firefox" } },
  ],
});
