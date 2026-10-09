import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";

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
  expect(channelWallpaper.equals(privateWallpaper)).toBe(true);

  await page.getByTestId("composer-more-tools").click();
  await expect(page.getByRole("toolbar", { name: "Tools" })).toBeVisible();
  const expandedWallpaper = await page.screenshot({ clip });
  expect(expandedWallpaper.equals(channelWallpaper)).toBe(true);
});
