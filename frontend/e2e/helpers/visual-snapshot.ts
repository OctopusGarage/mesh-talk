import type { Page } from "@playwright/test";

/**
 * Call before the UI action that opens the surface being snapshotted, then take the snapshot
 * immediately after the surface becomes visible. The overrides stay active for the rest of the
 * test, so use this only on pages you snapshot right away.
 */
export async function prepareForScreenshot(page: Page) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.addStyleTag({
    content: `
      * { caret-color: transparent !important; }
      html, body { scroll-behavior: auto !important; }
    `,
  });
  await page.evaluate(async () => {
    await (document as Document & { fonts?: FontFaceSet }).fonts?.ready;
  });
}

export function snapshotName(
  surface: string,
  theme: string,
  width: number,
  height: number,
) {
  return `${surface}-${theme}-${width}x${height}.png`;
}
