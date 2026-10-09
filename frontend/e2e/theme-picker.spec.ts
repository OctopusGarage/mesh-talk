import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { test, expect } from "./tauri-mock";
import { seedMarketPacks } from "./helpers/packs";
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
  await seedMarketPacks(page, ["barcelona"]);
  await login(page);
  const onlineColor = () =>
    page
      .locator('[data-testid="lan-online-count"] [role="status"] > span')
      .last()
      .evaluate((dot) => getComputedStyle(dot).backgroundColor);
  const darkOnline = await onlineColor();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
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
  await openSidebarMenuAction(page, "sidebar-nav-settings");

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

test("theme previews match the canvas, signal, and rail colors they apply", async ({
  page,
}) => {
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  for (const id of [
    "dark",
    "light",
    "oled",
    "argentina",
    "barcelona",
    "messi",
    "nature",
    "cat-acrylic",
  ]) {
    const preview = page.getByTestId(`theme-preview-${id}`);
    const previewCanvas = await preview.evaluate(
      (element) => getComputedStyle(element).backgroundColor,
    );
    await page.getByTestId(`theme-${id}`).click();
    const applied = await page.evaluate(() => {
      const root = getComputedStyle(document.documentElement);
      const sample = document.createElement("div");
      document.body.append(sample);
      const colors = ["--background", "--signal", "--shell-rail"].map(
        (token) => {
          sample.style.backgroundColor = `hsl(${root.getPropertyValue(token)})`;
          return getComputedStyle(sample).backgroundColor;
        },
      );
      sample.remove();
      return colors;
    });
    const previewColors = await preview.evaluate((element) => {
      const sample = document.createElement("div");
      document.body.append(sample);
      const colors = [
        element.dataset.previewSignal,
        element.dataset.previewRail,
      ].map((value) => {
        sample.style.backgroundColor = value ?? "";
        return getComputedStyle(sample).backgroundColor;
      });
      sample.remove();
      return colors;
    });
    expect([previewCanvas, ...previewColors], `${id} preview`).toEqual(applied);
  }
});

test("wallpaper can be hidden without changing the personal theme", async ({
  page,
}) => {
  await seedMarketPacks(page, ["barcelona"]);
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  const shell = page.getByTestId("chat-shell");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page.getByTestId("theme-barcelona").click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("data:image/");

  const wallpaper = page.getByTestId("settings-wallpaper");
  await wallpaper.click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toBe("none");
  await expect.poll(() => palette(page)).toBe("barcelona");
  await page.reload();
  await expect
    .poll(() => page.evaluate(() => document.documentElement.dataset.wallpaper))
    .toBe("off");

  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await wallpaper.click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("data:image/");
});

test("nature theme offers all 50 wallpapers and restores the selected scene", async ({
  page,
}) => {
  await seedMarketPacks(page, ["nature"]);
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page.getByTestId("theme-nature").click();

  const gallery = page.getByTestId("nature-wallpaper-picker");
  await expect(gallery).toBeVisible();
  await expect(gallery.getByRole("button")).toHaveCount(50);

  const selected = page.getByTestId("nature-wallpaper-050-maldives-wallpaper");
  await selected.click();
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(() =>
      page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toContain("data:image/");
  await expect
    .poll(() =>
      page
        .locator(".conversation-canvas")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toBe("none");
  const sidebarColor = await page
    .getByTestId("sidebar")
    .evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(sidebarColor).toMatch(/, 0\.74\)$/);
  await expect.poll(() => palette(page)).toBe("nature");

  await page.reload();
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-nature")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("settings-wallpaper").click();
  await expect
    .poll(() =>
      page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toBe("none");
});

test("Cat Acrylic theme offers 45 wallpapers and restores the selection", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page.getByTestId("theme-cat-acrylic").click();
  const gallery = page.getByTestId("cat-acrylic-wallpaper-picker");
  await expect(gallery.getByRole("button")).toHaveCount(45);
  const selected = gallery.getByRole("button", { name: "Cat 45" });
  await selected.click();
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => palette(page)).toBe("cat-acrylic");
  await expect
    .poll(() =>
      page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toContain("cat-45");

  await page.reload();
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-cat-acrylic")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(selected).toHaveAttribute("aria-pressed", "true");
});
