import { sourceIdentity } from "../../../scripts/evals/source-identity.mjs";
import { writeFile } from "node:fs/promises";
import { test as mocked, expect } from "../tauri-mock";
import {
  installBrowserErrorDetails,
  readBrowserErrorDetails,
} from "./browser-error-details";

export function requireNoUncaughtBrowserErrors(errors: readonly string[]) {
  expect(errors, "uncaught React/browser errors").toEqual([]);
}

// Browser evidence only: this fixture never starts the Rust/Tauri runtime.
export const test = mocked.extend<{ portableEvidence: void }>({
  portableEvidence: [
    async ({ page, browserName }, provide, testInfo) => {
      const errors: string[] = [];
      let fixtureFailed = false;
      let fixtureError: string | undefined;
      let screenshotError: string | undefined;
      const sourceSha = sourceIdentity();
      // Portable scenarios are offline and must not depend on the public catalog.
      // WebKit reports failed cross-origin catalog requests as page errors.
      await page.route("**/marketplace/catalog.json", (route) =>
        route.fulfill({
          contentType: "application/json",
          headers: { "access-control-allow-origin": "*" },
          body: JSON.stringify({ schemaVersion: 1, packs: [] }),
        }),
      );
      page.on("pageerror", (error) => errors.push(error.message));
      try {
        await installBrowserErrorDetails(page);
        expect(sourceSha).toMatch(/^[a-f0-9]{40}$/);
        if (process.env.EVAL_SOURCE_SHA !== undefined)
          expect(
            process.env.EVAL_SOURCE_SHA,
            "evidence source must match actual checkout",
          ).toBe(sourceSha);
        await provide();
        requireNoUncaughtBrowserErrors(errors);
      } catch (error) {
        fixtureFailed = true;
        fixtureError = String(error);
        throw error;
      } finally {
        const screenshotPath = testInfo.outputPath("screenshot.png");
        try {
          await page.screenshot({ path: screenshotPath });
          await testInfo.attach("portable-browser-screenshot", {
            path: screenshotPath,
            contentType: "image/png",
          });
        } catch (error) {
          screenshotError = String(error);
        }
        const evidencePath = testInfo.outputPath("evidence.json");
        const browserErrors = await readBrowserErrorDetails(page);
        if (errors.length) {
          // Keep the first causes in job logs as well as the complete bounded
          // attachment, so diagnosing a startup failure needs no archive download.
          process.stderr.write(
            `Portable browser error details:\n${JSON.stringify({ ...browserErrors, errors: browserErrors.errors.slice(0, 3) }, null, 2)}\n`,
          );
        }
        await writeFile(
          evidencePath,
          JSON.stringify({
            platform: process.platform,
            browser: browserName,
            sourceSha,
            mocked: true,
            native: false,
            scenario: testInfo.title,
            scenarioPath: testInfo.titlePath,
            status:
              fixtureFailed || screenshotError ? "failed" : testInfo.status,
            screenshotError,
            fixtureError,
            viewport: page.viewportSize(),
            pageErrors: errors,
            browserErrorDetails: browserErrors.errors,
            browserErrorDetailsTruncated: browserErrors.truncated,
            browserErrorDetailsReadError: browserErrors.readError,
          }),
        );
        await testInfo.attach("portable-browser-evidence", {
          path: evidencePath,
          contentType: "application/json",
        });
      }
      if (screenshotError) throw new Error(screenshotError);
    },
    { auto: true },
  ],
});
export { expect };
