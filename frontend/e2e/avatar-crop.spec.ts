import { test, expect } from "./tauri-mock";

// Regression: the avatar crop dialog's main viewport must actually SHOW the picked image
// (not just the tiny preview). It once rendered <img src={img.src}> where loadImage had
// already revoked that object URL → the viewport was blank (naturalWidth 0). The dialog
// now holds its own live object URL for the viewport.
// A repo image, referenced RELATIVE to the Playwright cwd (frontend/) so it resolves on
// every CI runner — an absolute path would only exist on the author's machine.
const IMG = "../src-tauri/icons/icon.png";
test.use({ viewport: { width: 1100, height: 800 } });

test("avatar crop viewport displays the picked image", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  // Avatar editing now lives in the profile dialog (opened from the identity header).
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page
      .getByText(/Set photo|Change photo/)
      .first()
      .click(),
  ]);
  await chooser.setFiles(IMG);
  await expect(page.getByText("Crop photo")).toBeVisible();

  // The viewport <img> must have actually loaded (a revoked URL leaves naturalWidth 0).
  const vp = page.getByRole("application").locator("img");
  await expect
    .poll(async () => vp.evaluate((el: HTMLImageElement) => el.naturalWidth))
    .toBeGreaterThan(0);
});
