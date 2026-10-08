import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });

async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
}
const palette = (page: import("@playwright/test").Page) =>
  page.evaluate(() => document.documentElement.getAttribute("data-palette"));

test("theme picker applies + persists a brand palette, and clears it for base modes", async ({
  page,
}) => {
  await login(page);
  const onlineColor = () =>
    page
      .locator('[data-testid="lan-online-count"] [role="status"] > span')
      .last()
      .evaluate((dot) => getComputedStyle(dot).backgroundColor);
  const darkOnline = await onlineColor();
  await page.getByTestId("sidebar-nav-settings").click();
  await expect(page.getByTestId("theme-picker")).toBeVisible();

  await page.getByTestId("theme-barcelona").click();
  await expect.poll(() => palette(page)).toBe("barcelona");
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-theme")),
  ).toBe("barcelona");
  // The brand crest surfaces in the footer mark while the theme is active.
  await expect(page.getByTestId("theme-crest")).toBeVisible();
  expect(await onlineColor()).toBe(darkOnline);

  await page.getByTestId("theme-dark").click();
  await expect.poll(() => palette(page)).toBe(null); // base mode clears the palette
  await expect(page.getByTestId("theme-crest")).toBeHidden(); // back to the app mark
});

test("theme changes stay scoped and reduced motion keeps control feedback", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("sidebar-nav-settings").click();

  const normal = await page.getByTestId("theme-light").evaluate((button) => {
    (button as HTMLButtonElement).click();
    const root = document.documentElement;
    const sidebar = document.querySelector(".sidebar-container");
    return {
      transitioning: root.classList.contains("theme-transitioning"),
      sidebarProperty: sidebar && getComputedStyle(sidebar).transitionProperty,
      injectedUniversalRule: [...document.head.querySelectorAll("style")].some(
        (style) =>
          style.textContent?.includes("*,*::before,*::after{transition:"),
      ),
    };
  });
  expect(normal).toEqual({
    transitioning: true,
    sidebarProperty: "background-color",
    injectedUniversalRule: false,
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.documentElement.classList.contains("theme-transitioning"),
      ),
    )
    .toBe(false);

  await page.emulateMedia({ reducedMotion: "reduce" });
  const reduced = await page.getByTestId("theme-dark").evaluate((button) => {
    (button as HTMLButtonElement).click();
    const style = getComputedStyle(button);
    return {
      transitioning: document.documentElement.classList.contains(
        "theme-transitioning",
      ),
      properties: style.transitionProperty,
      duration: style.transitionDuration,
    };
  });
  expect(reduced.transitioning).toBe(false);
  expect(reduced.properties).toContain("background-color");
  expect(reduced.properties).not.toContain("transform");
  expect(reduced.duration).toBe("0.08s");
});

test("wallpaper can be hidden without changing the personal theme", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  const canvas = page.locator(".conversation-canvas");
  await page.getByTestId("sidebar-nav-settings").click();
  await page.getByTestId("theme-barcelona").click();
  await expect
    .poll(() => canvas.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("barcelona-bg");

  const wallpaper = page.getByTestId("settings-wallpaper");
  await wallpaper.click();
  await expect
    .poll(() => canvas.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toBe("none");
  await expect.poll(() => palette(page)).toBe("barcelona");
  await page.reload();
  await expect
    .poll(() => page.evaluate(() => document.documentElement.dataset.wallpaper))
    .toBe("off");

  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("sidebar-nav-settings").click();
  await wallpaper.click();
  await expect
    .poll(() => canvas.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("barcelona-bg");
});
