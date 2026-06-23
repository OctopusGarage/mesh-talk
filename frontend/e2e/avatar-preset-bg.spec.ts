import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 900, height: 700 } });

// A preset player photo is a transparent cutout; JPEG has no alpha, so without a light fill
// its background flattens to BLACK. The chosen avatar's backdrop must be LIGHT, not black.
test("a chosen preset avatar has a light background (not black)", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await page.getByTestId("avatar-gallery").locator("button").first().click();

  await expect
    .poll(async () =>
      page.evaluate(() => {
        const img = document.querySelector(
          "header img, aside img",
        ) as HTMLImageElement | null;
        if (!img?.naturalWidth) return null;
        const c = document.createElement("canvas");
        c.width = img.naturalWidth;
        c.height = img.naturalHeight;
        const ctx = c.getContext("2d")!;
        ctx.drawImage(img, 0, 0);
        const [r, g, b] = ctx.getImageData(2, 2, 1, 1).data;
        return Math.min(r, g, b); // darkest channel of the corner pixel
      }),
    )
    .toBeGreaterThan(200); // light fill (~241), not black (0)
});
