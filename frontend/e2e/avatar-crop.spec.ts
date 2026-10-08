import { test, expect } from "./tauri-mock";

// Regression: the avatar crop dialog's main viewport must actually SHOW the picked image
// (not just the tiny preview). It once rendered <img src={img.src}> where loadImage had
// already revoked that object URL → the viewport was blank (naturalWidth 0). The dialog
// now holds its own live object URL for the viewport.
// A repo image, referenced RELATIVE to the Playwright cwd (frontend/) so it resolves on
// every CI runner — an absolute path would only exist on the author's machine.
const IMG = "../src-tauri/icons/icon.png";
test.use({ viewport: { width: 1100, height: 800 } });

async function openCrop(page: import("@playwright/test").Page) {
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
}

test("avatar crop viewport displays the picked image", async ({ page }) => {
  await openCrop(page);

  // The viewport <img> must have actually loaded (a revoked URL leaves naturalWidth 0).
  const vp = page.getByRole("application").locator("img");
  await expect
    .poll(async () => vp.evaluate((el: HTMLImageElement) => el.naturalWidth))
    .toBeGreaterThan(0);
});

test("keyboard can reposition the zoomed photo", async ({ page }) => {
  await openCrop(page);
  const viewport = page.getByRole("application");
  const photo = viewport.locator("img");
  await page.getByRole("slider", { name: /zoom/i }).fill("2");
  await viewport.focus();
  const before = await photo.evaluate(
    (el) => (el as HTMLElement).style.transform,
  );
  await viewport.press("ArrowRight");
  const after = await photo.evaluate(
    (el) => (el as HTMLElement).style.transform,
  );
  expect(after).not.toBe(before);
  await viewport.press("Shift+ArrowLeft");
  const shifted = await photo.evaluate(
    (el) => (el as HTMLElement).style.transform,
  );
  expect(shifted).not.toBe(after);
  await viewport.press("Tab");
  await expect(page.getByRole("slider", { name: /zoom/i })).toBeFocused();
});

test("a second touch cannot take over the crop drag", async ({ page }) => {
  await openCrop(page);
  const viewport = page.getByRole("application");
  const photo = viewport.locator("img");
  await page.getByRole("slider", { name: /zoom/i }).fill("2");
  const box = await viewport.boundingBox();
  if (!box) throw new Error("Missing crop viewport");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const session = await page.context().newCDPSession(page);
  const point = (id: number, px: number) => ({
    id,
    x: px,
    y,
    radiusX: 4,
    radiusY: 4,
  });
  const touch = (
    type: "touchStart" | "touchMove" | "touchEnd",
    points: ReturnType<typeof point>[],
  ) => session.send("Input.dispatchTouchEvent", { type, touchPoints: points });
  const position = () =>
    photo.evaluate((el) =>
      Number(
        (el as HTMLElement).style.transform.match(
          /translate\(([-\d.]+)px/,
        )?.[1],
      ),
    );

  await touch("touchStart", [point(1, x)]);
  await touch("touchMove", [point(1, x + 30)]);
  const first = await position();
  await touch("touchStart", [point(1, x + 30), point(2, x + 80)]);
  await touch("touchMove", [point(1, x + 40), point(2, x + 80)]);
  expect(await position()).toBe(first + 10);
  await touch("touchEnd", [point(2, x + 80)]);
  const ended = await position();
  await touch("touchMove", [point(2, x + 100)]);
  expect(await position()).toBe(ended);
  await touch("touchEnd", []);

  await touch("touchStart", [point(3, x)]);
  await touch("touchMove", [point(3, x + 10)]);
  expect(await position()).toBeGreaterThan(ended);
  await touch("touchEnd", []);
});
