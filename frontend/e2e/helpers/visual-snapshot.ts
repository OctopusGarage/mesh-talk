import type { Locator, Page } from "@playwright/test";

/** Native Windows UI fonts depend on the OS language. Keep reviewed references
 * for the actual rendered Latin font, with the same strict pixel threshold. */
export async function windowsSnapshotProfile(sample: Locator) {
  const page = sample.page();
  const session = await page.context().newCDPSession(page);
  await sample.evaluate((element) =>
    element.setAttribute("data-snapshot-font", ""),
  );
  try {
    await session.send("DOM.enable");
    await session.send("CSS.enable");
    const { root } = await session.send("DOM.getDocument");
    const { nodeId } = await session.send("DOM.querySelector", {
      nodeId: root.nodeId,
      selector: "[data-snapshot-font]",
    });
    const { fonts } = await session.send("CSS.getPlatformFontsForNode", {
      nodeId,
    });
    const families = fonts
      .filter((font) => font.glyphCount > 0)
      .map((font) => font.familyName);
    if (!families.length)
      throw new Error("Snapshot sample has no rendered font");
    console.info("Windows snapshot font:", families.join(", "));
    return families.some((name) => name.startsWith("Microsoft YaHei"))
      ? "win32-yahei"
      : "win32";
  } finally {
    await session.detach();
    await sample.evaluate((element) =>
      element.removeAttribute("data-snapshot-font"),
    );
  }
}

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
