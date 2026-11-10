import { test, expect } from "@playwright/test";
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
    route.fulfill({ status: 503, body: "Offline fixture" }),
  );
});

test("visible topology demonstrates packets traveling along its connections", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(hosted.url);
  const figure = page.locator(".network-proof");
  await figure.scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "playing");
  const packet = figure.locator("[data-flow-packet]").first();
  const initial = await packet.evaluate(
    (element) => getComputedStyle(element).transform,
  );
  await expect
    .poll(() =>
      packet.evaluate((element) => getComputedStyle(element).transform),
    )
    .not.toBe(initial);
  await expect(
    figure.getByRole("button", { name: "Pause data flow", exact: true }),
  ).toBeVisible();
});

test("pause persists through viewport exit and replay starts a new demonstration", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(hosted.url);
  const figure = page.locator(".network-proof");
  await figure.scrollIntoViewIfNeeded();
  await figure
    .getByRole("button", { name: "Pause data flow", exact: true })
    .click();
  await expect(figure).toHaveAttribute("data-flow-state", "paused");
  expect(
    await figure.evaluate((el) =>
      el
        .getAnimations({ subtree: true })
        .some((a) => a.playState === "running"),
    ),
  ).toBe(false);
  await page.locator("h1").scrollIntoViewIfNeeded();
  await expect
    .poll(() =>
      figure.evaluate((el) => el.getAnimations({ subtree: true }).length),
    )
    .toBe(0);
  await figure.scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "paused");
  await figure
    .getByRole("button", { name: "Replay data flow", exact: true })
    .click();
  await expect(figure).toHaveAttribute("data-flow-state", "playing");
});

test("leaving the viewport removes all traffic animations and reentry resumes", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(hosted.url);
  const figure = page.locator(".network-proof");
  await figure.scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "playing");
  await page.locator("h1").scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "offscreen");
  expect(
    await figure.evaluate((el) => el.getAnimations({ subtree: true }).length),
  ).toBe(0);
  await figure.scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "playing");
});

test("reduced motion preserves the full static topology and cancels a live stream", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(hosted.url);
  const figure = page.locator(".network-proof");
  await figure.scrollIntoViewIfNeeded();
  await expect(figure).toHaveAttribute("data-flow-state", "reduced");
  await expect(figure.locator("svg[role=img]")).toBeVisible();
  expect(
    await figure.evaluate((el) => el.getAnimations({ subtree: true }).length),
  ).toBe(0);
  await expect(figure.locator(".flow-controls")).toBeHidden();
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await expect(figure).toHaveAttribute("data-flow-state", "playing");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(figure).toHaveAttribute("data-flow-state", "reduced");
  expect(
    await figure.evaluate((el) => el.getAnimations({ subtree: true }).length),
  ).toBe(0);
});
