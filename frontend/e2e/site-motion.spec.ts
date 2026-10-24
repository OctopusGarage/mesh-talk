import { expect, test } from "@playwright/test";
import { servePublicSite } from "./helpers/public-site";

let hosted: Awaited<ReturnType<typeof servePublicSite>>;
test.beforeAll(async () => {
  hosted = await servePublicSite();
});
test.afterAll(async () => {
  await hosted?.close();
});
test.beforeEach(async ({ page }) => {
  await page.route("https://api.github.com/repos/**", (route) =>
    route.fulfill({ status: 503, json: { message: "Offline fixture" } }),
  );
});

for (const reducedMotion of ["no-preference", "reduce"] as const) {
  test(`preview and gallery stay usable with ${reducedMotion} motion`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion });
    await page.goto(hosted.url);
    const preview = page.locator("#customization-image");
    await expect(page.locator("h1")).toBeVisible();
    await page.locator("#theme-barcelona").click();
    await expect(preview).toHaveAttribute("src", /hero-barcelona\.webp$/);
    await page.locator("#theme-argentina").click();
    await expect(preview).toHaveAttribute("src", /hero-argentina\.webp$/);

    const opener = page.locator(".customization-link");
    await opener.click();
    const viewer = page.locator(".image-viewer");
    await expect(viewer).toBeVisible();
    await expect(viewer.locator("img")).toHaveAttribute(
      "src",
      /hero-argentina@2x\.webp$/,
    );
    const position = await viewer.locator(".viewer-position").textContent();
    await page.keyboard.press("ArrowRight");
    await expect(viewer.locator(".viewer-position")).not.toHaveText(position!);
    // Closing during a transition must immediately release the native modal
    // and restore its opener, with no delayed animation resurrecting it.
    await page.keyboard.press("Escape");
    await expect(viewer).toBeHidden();
    await expect(opener).toBeFocused();
    await opener.click();
    await expect(viewer).toBeVisible();
    await expect(viewer.locator("img")).toHaveAttribute(
      "src",
      /hero-argentina@2x\.webp$/,
    );
    await page.keyboard.press("Escape");
    await expect(opener).toBeFocused();

    await expect
      .poll(() => page.evaluate(() => document.getAnimations().length))
      .toBe(0);
    await expect(preview).toHaveCSS("opacity", "1");
    await expect(preview).toHaveCSS("transform", "none");
  });
}

test("a live reduced-motion preference cancels arrivals and future transitions", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(hosted.url);
  await page.locator(".preview-link").click();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect
    .poll(() => page.evaluate(() => document.getAnimations().length))
    .toBe(0);
  const viewer = page.locator(".image-viewer");
  await expect(viewer).toHaveCSS("transform", "none");
  await page.keyboard.press("ArrowRight");
  await expect(viewer.locator(".viewer-position")).toHaveText(/^2 \/ /);
  await expect(viewer.locator("img")).toHaveCSS("transform", "none");
  expect(await page.evaluate(() => document.getAnimations().length)).toBe(0);
  await page.keyboard.press("Escape");
  await expect(page.locator(".preview-link")).toBeFocused();
});

test("a native gallery arrival is bounded and does not delay dismissal", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(hosted.url);
  const animations = await page
    .locator(".preview-link")
    .evaluate(async (link) => {
      (link as HTMLAnchorElement).click();
      await new Promise(requestAnimationFrame);
      return document
        .querySelector(".image-viewer")!
        .getAnimations()
        .map((animation) => animation.effect!.getComputedTiming());
    });
  expect(animations.length).toBeGreaterThan(0);
  for (const timing of animations) {
    expect(timing.iterations).toBe(1);
    expect(Number(timing.endTime)).toBeLessThanOrEqual(800);
  }
  await page.keyboard.press("Escape");
  await expect(page.locator(".image-viewer")).toBeHidden();
  await expect(page.locator(".preview-link")).toBeFocused();
});

test("late image loading cannot restore an outdated theme or leave motion running", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  let releaseImage!: () => void;
  const holdImage = new Promise<void>((resolve) => {
    releaseImage = resolve;
  });
  await page.route("**/screenshots/hero-barcelona.webp", async (route) => {
    await holdImage;
    await route.continue();
  });
  await page.goto(hosted.url);
  await page.locator("#theme-barcelona").click();
  await page.locator("#theme-argentina").click();
  releaseImage();
  const preview = page.locator("#customization-image");
  await expect(preview).toHaveAttribute("src", /hero-argentina\.webp$/);
  await expect
    .poll(() => preview.evaluate((image: HTMLImageElement) => image.complete))
    .toBe(true);
  await expect
    .poll(() => page.evaluate(() => document.getAnimations().length))
    .toBe(0);
  await expect(preview).toHaveCSS("opacity", "1");
  await expect(preview).toHaveCSS("transform", "none");
  await expect(page.locator("#theme-argentina")).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("a pending theme keeps the previous complete preview and selection", async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/screenshots/hero-barcelona.webp", async (route) => {
    await held;
    await route.continue();
  });
  await page.goto(hosted.url);
  await page.locator("#theme-barcelona").click();
  await expect(page.locator("#customization-panel")).toHaveAttribute(
    "aria-busy",
    "true",
  );
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    /hero-messi\.webp$/,
  );
  await expect(page.locator("#theme-messi")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.locator("#theme-argentina").click();
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    /hero-argentina\.webp$/,
  );
  release();
  await expect(page.locator("#theme-argentina")).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("failed previews retain the current image and can be retried", async ({
  page,
}) => {
  await page.route("**/screenshots/hero-barcelona.webp", (route) =>
    route.fulfill({ status: 404, body: "Missing fixture" }),
  );
  await page.goto(hosted.url);
  await page.locator("#theme-barcelona").click();
  await expect(page.locator(".preview-status")).toContainText("couldn’t load");
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    /hero-messi\.webp$/,
  );
  await page.unroute("**/screenshots/hero-barcelona.webp");
  await page.locator("#theme-barcelona").click();
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    /hero-barcelona\.webp$/,
  );
  await expect(page.locator(".preview-status")).toHaveText("");
});
