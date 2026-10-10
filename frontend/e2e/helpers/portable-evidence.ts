import { execFileSync } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { test as mocked, expect } from "../tauri-mock";

// Browser evidence only: this fixture never starts the Rust/Tauri runtime.
export const test = mocked.extend<{ portableEvidence: void }>({
  portableEvidence: [
    async ({ page, browserName }, provide, testInfo) => {
      const errors: string[] = [];
      let fixtureFailed = false;
      let fixtureError: string | undefined;
      let screenshotError: string | undefined;
      const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], {
        encoding: "utf8",
      }).trim();
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
        expect(sourceSha).toMatch(/^[a-f0-9]{40}$/);
        if (process.env.EVAL_SOURCE_SHA !== undefined)
          expect(
            process.env.EVAL_SOURCE_SHA,
            "evidence source must match actual checkout",
          ).toBe(sourceSha);
        await provide();
        expect(errors, "uncaught React/browser errors").toEqual([]);
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
