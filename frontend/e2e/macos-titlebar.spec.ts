import { expect, test } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";

test.describe("macOS overlay titlebar", () => {
  test.use({
    userAgent:
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
  });

  for (const viewport of [
    { width: 760, height: 520 },
    { width: 1040, height: 720 },
  ]) {
    test(`profile clears traffic lights at ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await page.setViewportSize(viewport);
      await enterChat(page);
      await expect(page.locator("html")).toHaveAttribute("data-os", "macos");
      const avatar = page.getByTestId("open-profile");
      await expect(page.getByTestId("self-identity")).toHaveCSS(
        "padding-top",
        "28px",
      );
      const bounds = await avatar.boundingBox();
      expect(bounds).not.toBeNull();
      expect(bounds!.y).toBeGreaterThanOrEqual(28);
      // Check the avatar's top edge, not just its center: the old layout's upper
      // portion was covered by both traffic lights and the invisible drag strip.
      expect(
        await avatar.evaluate((element) => {
          const rect = element.getBoundingClientRect();
          return element.contains(
            document.elementFromPoint(rect.x + rect.width / 2, rect.y + 1),
          );
        }),
      ).toBe(true);
      await avatar.click({ position: { x: 19, y: 1 } });
      await expect(page.getByRole("dialog")).toBeVisible();
      await page.keyboard.press("Escape");
      await openBobDm(page);
      await expect(page.getByTestId("conversation-header")).toHaveCSS(
        "padding-top",
        "28px",
      );
    });
  }
});

for (const [platform, userAgent] of [
  ["Windows", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chromium/120"],
  ["Linux", "Mozilla/5.0 (X11; Linux x86_64) Chromium/120"],
]) {
  test.describe(`${platform} sidebar`, () => {
    test.use({ userAgent });
    test("does not add macOS top spacing", async ({ page }) => {
      await enterChat(page);
      await expect(page.getByTestId("self-identity")).toHaveCSS(
        "padding-top",
        "12px",
      );
      await page.getByTestId("open-profile").click();
      await expect(page.getByRole("dialog")).toBeVisible();
    });
  });
}
