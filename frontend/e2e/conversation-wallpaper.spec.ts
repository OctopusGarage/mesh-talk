import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";

// GPU gradient compositing can round an individual color channel by one level
// between captures. Compare rendered pixels so this test still catches movement.
async function maxPixelDrift(
  page: import("@playwright/test").Page,
  a: Buffer,
  b: Buffer,
) {
  return page.evaluate(
    async ([first, second]) => {
      const pixels = async (base64: string) => {
        const image = await createImageBitmap(
          await (await fetch(`data:image/png;base64,${base64}`)).blob(),
        );
        const canvas = document.createElement("canvas");
        canvas.width = image.width;
        canvas.height = image.height;
        const context = canvas.getContext("2d");
        if (!context) throw new Error("Canvas unavailable");
        context.drawImage(image, 0, 0);
        const data = context.getImageData(0, 0, image.width, image.height).data;
        image.close();
        return data;
      };
      const left = await pixels(first);
      const right = await pixels(second);
      if (left.length !== right.length)
        throw new Error("Capture dimensions changed");
      let max = 0;
      for (let i = 0; i < left.length; i++)
        max = Math.max(max, Math.abs(left[i] - right[i]));
      return max;
    },
    [a.toString("base64"), b.toString("base64")],
  );
}

for (const theme of ["argentina", "barcelona", "messi"] as const) {
  test(`${theme} theme shows one image across the chat shell`, async ({
    page,
  }) => {
    await seedThemeBeforeLoad(page, theme);
    await enterChat(page);
    const homeBackground = await page
      .getByTestId("chat-shell")
      .evaluate((element) => getComputedStyle(element).backgroundImage);
    expect(homeBackground.includes("data:image/")).toBe(true);
    await openBobDm(page);

    const frame = page.locator("main.conversation-canvas");
    const frameBackground = await frame.evaluate(
      (element) => getComputedStyle(element).backgroundImage,
    );
    expect(frameBackground).toBe("none");
    const background = await page
      .getByTestId("chat-shell")
      .evaluate((element) => getComputedStyle(element).backgroundImage);
    expect(background.includes("data:image/")).toBe(true);
    expect(background).toContain("linear-gradient");
    await expect(page.locator(".conversation-log-surface")).toHaveCSS(
      "background-color",
      "rgba(0, 0, 0, 0)",
    );

    const sidebarBackground = await page
      .getByTestId("sidebar")
      .evaluate((element) => getComputedStyle(element).backgroundColor);
    expect(sidebarBackground).toMatch(/, 0\.74\)$/);
  });
}

test("base theme keeps a plain conversation canvas", async ({ page }) => {
  await enterChat(page);
  const homeBackground = await page
    .getByTestId("chat-shell")
    .evaluate((element) => getComputedStyle(element).backgroundImage);
  expect(homeBackground).toBe("none");
  await openBobDm(page);

  const background = await page
    .locator("main.conversation-canvas")
    .evaluate((element) => getComputedStyle(element).backgroundImage);
  expect(background).toBe("none");
});

test("wallpaper stays in place across conversation and composer changes", async ({
  page,
}) => {
  await seedThemeBeforeLoad(page, "barcelona");
  await enterChat(page);
  await openBobDm(page);
  const main = await page.locator("main").boundingBox();
  if (!main) throw new Error("Missing conversation surface");
  const clip = {
    x: main.x + 36,
    y: main.y + 150,
    width: 96,
    height: 96,
  };
  const privateWallpaper = await page.screenshot({ clip });

  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await expect(page.getByTestId("conversation-header")).toContainText("team");
  await expect(page.getByRole("log")).toBeVisible();
  const channelWallpaper = await page.screenshot({ clip });
  expect(
    await maxPixelDrift(page, channelWallpaper, privateWallpaper),
  ).toBeLessThanOrEqual(1);

  await page.getByTestId("composer-more-tools").click();
  await expect(page.getByRole("toolbar", { name: "Tools" })).toBeVisible();
  const expandedWallpaper = await page.screenshot({ clip });
  expect(
    await maxPixelDrift(page, expandedWallpaper, channelWallpaper),
  ).toBeLessThanOrEqual(1);
});
