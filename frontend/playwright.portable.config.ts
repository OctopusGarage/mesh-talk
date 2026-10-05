import { defineConfig } from "@playwright/test";

const browser = process.env.EVAL_BROWSER ?? "chromium";
const tier = process.env.EVAL_TIER ?? "core";
const evidenceRoot = `eval-results/portable/${browser}/${tier}`;
if (!["chromium", "webkit", "firefox"].includes(browser))
  throw new Error(`Unsupported EVAL_BROWSER: ${browser}`);
if (!["core", "extended"].includes(tier))
  throw new Error(`Unsupported EVAL_TIER: ${tier}`);
const defaultPort =
  5174 +
  ["chromium", "webkit", "firefox"].indexOf(browser) * 2 +
  (tier === "extended" ? 1 : 0);
const rawPort = process.env.EVAL_UI_PORT ?? String(defaultPort);
const port = Number(rawPort);
if (
  !/^\d+$/.test(rawPort) ||
  !Number.isSafeInteger(port) ||
  port < 1 ||
  port > 65535
)
  throw new Error(`Invalid EVAL_UI_PORT: ${rawPort}`);
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./e2e",
  testMatch: [
    "portable-core.spec.ts",
    "automatic-delivery.spec.ts",
    "media-recovery.spec.ts",
    "media-lifecycle.spec.ts",
  ],
  grepInvert: tier === "core" ? /@extended/ : undefined,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [
    ["list"],
    ["json", { outputFile: `${evidenceRoot}/results.json` }],
  ],
  outputDir: `${evidenceRoot}/artifacts`,
  use: { baseURL, trace: "on" },
  projects: [
    {
      name: browser,
      use: {
        browserName: browser as "chromium" | "webkit" | "firefox",
        viewport: { width: 1280, height: 720 },
      },
    },
  ],
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${port} --strictPort`,
    url: baseURL,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
