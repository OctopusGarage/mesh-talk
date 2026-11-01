import { expect, test } from "./tauri-mock";

test.use({
  userAgent:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
});

for (const viewport of [
  { width: 1040, height: 720 },
  { width: 760, height: 520 },
]) {
  test(`Mac authentication actions fit ${viewport.width}x${viewport.height}`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await page.goto("/");
    await expect(page.getByTestId("mac-auth-guidance")).toBeVisible();
    for (const tab of ["signin", "register"]) {
      await page.getByTestId(`login-tab-${tab}`).click();
      await expect(page.getByTestId("login-submit")).toBeInViewport({
        ratio: 1,
      });
    }
  });
}
