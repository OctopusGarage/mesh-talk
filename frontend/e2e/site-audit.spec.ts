import { servePublicSite } from "./helpers/public-site";
import AxeBuilder from "@axe-core/playwright";
import { test, expect } from "@playwright/test";

let site: string;
let hosted: Awaited<ReturnType<typeof servePublicSite>>;
test.beforeAll(async () => {
  hosted = await servePublicSite();
  site = hosted.url;
});
test.afterAll(async () => {
  await hosted?.close();
});
// The static-layout audit exercises the offline fallback. Direct release lookup
// and native downloads have separate deterministic fixtures in site-downloads.
test.beforeEach(async ({ page }) => {
  await page.route(
    "https://api.github.com/repos/OctopusGarage/mesh-talk/releases/latest",
    (route) =>
      route.fulfill({
        status: 503,
        body: "Offline",
        headers: { "access-control-allow-origin": "*" },
      }),
  );
});

for (const width of [320, 390, 721, 1280]) {
  test(`static site is readable and accessible at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.goto(site);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Across LANs, when you choose." }),
    ).toBeVisible();
    await expect(page.locator(".marketplace-grid img")).toHaveCount(3);
    for (const image of await page
      .locator(".marketplace-grid img, .security-shot img")
      .all()) {
      await image.scrollIntoViewIfNeeded();
      await expect
        .poll(() =>
          image.evaluate(
            (element) => (element as HTMLImageElement).naturalWidth,
          ),
        )
        .toBeGreaterThan(0);
    }
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            document.documentElement.scrollWidth <=
            document.documentElement.clientWidth + 1,
        ),
      )
      .toBe(true);

    for (const link of await page.locator(".foot__links a").all()) {
      const bounds = await link.boundingBox();
      expect(
        bounds,
        (await link.textContent()) ?? "Footer link",
      ).not.toBeNull();
      expect(bounds!.x).toBeGreaterThanOrEqual(0);
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width + 1);
    }

    const { violations } = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
      .analyze();
    expect(
      violations.map((violation) => ({
        id: violation.id,
        nodes: violation.nodes.map((node) => ({
          target: node.target,
          summary: node.failureSummary,
        })),
      })),
    ).toEqual([]);
  });
}

test("product preview supports keyboard selection and full-size viewing", async ({
  page,
}) => {
  await page.goto(site);
  await expect(page.locator("#tour [role=tablist]")).toHaveCount(0);
  await expect(page.locator("#preview-image")).toHaveAttribute(
    "src",
    "screenshots/app-conversations.webp",
  );
  const tabs = page.getByRole("tablist", {
    name: "Marketplace theme examples",
  });
  await tabs.getByRole("tab", { name: "Barcelona", exact: true }).click();
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    "screenshots/hero-barcelona.webp",
  );
  await page.keyboard.press("ArrowRight");
  await expect(
    tabs.getByRole("tab", { name: "Argentina", exact: true }),
  ).toBeFocused();
  await expect(page.locator("#customization-image")).toHaveAttribute(
    "src",
    "screenshots/hero-argentina.webp",
  );
  await page
    .getByRole("link", { name: "Enlarge Marketplace theme preview" })
    .click();
  await expect(
    page.getByRole("dialog", { name: "Screenshot viewer" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(
    page.getByRole("link", { name: "Enlarge Marketplace theme preview" }),
  ).toBeFocused();
});

test("mobile navigation, platform choices and FAQs are usable", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(site);
  const menu = page.getByRole("button", { name: "Menu", exact: true });
  await menu.click();
  await expect(menu).toHaveAttribute("aria-expanded", "true");
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Download", exact: true })
    .click();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await page.getByRole("tab", { name: "Linux", exact: true }).click();
  await expect(
    page.getByRole("tabpanel", { name: "Linux", exact: true }),
  ).toContainText("AppImage");
  await page
    .getByText("Does it work without the internet?", { exact: true })
    .click();
  await expect(page.locator("#faq details").first()).toHaveAttribute(
    "open",
    "",
  );
});

test("landing content remains usable without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 320, height: 800 },
  });
  try {
    const page = await context.newPage();
    await page.goto(site);
    await expect(
      page
        .getByRole("navigation", { name: "Main navigation" })
        .getByRole("link", { name: "Features", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Get Mesh-Talk.", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: "Enlarge app preview" }),
    ).toHaveAttribute("href", "screenshots/app-conversations@2x.webp");
    await expect(page.locator("#download")).toContainText("AppImage");
  } finally {
    await context.close();
  }
});

test("reduced motion keeps the full product story visible", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(site);
  await expect(page.locator("#preview-image")).toBeVisible();
  expect(
    await page.evaluate(
      () => getComputedStyle(document.documentElement).scrollBehavior,
    ),
  ).toBe("auto");
});

test("screenshot viewer supports consecutive images, keyboard navigation and focus return", async ({
  page,
}) => {
  await page.goto(site);
  const opener = page.getByRole("link", { name: "Enlarge app preview" });
  await opener.click();
  const viewer = page.getByRole("dialog", { name: "Screenshot viewer" });
  await expect(viewer.locator("img")).toHaveAttribute(
    "src",
    /app-conversations@2x\.webp$/,
  );
  const first = await viewer.locator("img").getAttribute("src");
  await viewer.getByRole("button", { name: "Next screenshot" }).click();
  await expect(viewer.locator("img")).not.toHaveAttribute("src", first!);
  await page.keyboard.press("ArrowLeft");
  await expect(viewer.locator("img")).toHaveAttribute("src", first!);
  await expect(
    viewer.getByRole("link", { name: "Open original image" }),
  ).toHaveAttribute("href", first!);
  await page.keyboard.press("Escape");
  await expect(opener).toBeFocused();
});

test("FAQ deep links open the answer and source instructions can be copied", async ({
  page,
}) => {
  await page.goto(site + "#faq-offline");
  await expect(page.locator("#faq-offline")).toHaveAttribute("open", "");
  await page.getByRole("link", { name: "Link to this answer" }).first().click();
  await expect(page).toHaveURL(/#faq-offline$/);
  await page.getByText("Build and run from source", { exact: true }).click();
  await expect(page.locator("#source-command")).toContainText("make tauri-dev");
  await page.getByRole("button", { name: "Copy source commands" }).click();
  await expect(page.locator("#source-copy-status")).toHaveText(
    /copied|selected/,
  );
});

test("mobile navigation transfers focus to the destination", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(site);
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Features", exact: true })
    .click();
  await expect(page.locator("#features")).toBeFocused();
});

test("unknown Pages routes offer working recovery paths without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 390, height: 844 },
  });
  try {
    const page = await context.newPage();
    const response = await page.goto(new URL("missing/old-link", site).href);
    expect(response?.status()).toBe(404);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Let’s get you back in the conversation.",
    );
    await expect(
      page.getByRole("link", { name: "Browse the user guide" }),
    ).toHaveAttribute("href", "/mesh-talk/help/index.html");
    await page.getByRole("link", { name: "Back to home", exact: true }).click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Your conversations.On your terms.",
    );
  } finally {
    await context.close();
  }
});

test("200 percent text sizing preserves mobile headings and controls", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(site);
  await page.evaluate(() => {
    document.documentElement.style.fontSize = "200%";
  });
  const overflowing = await page
    .locator("h1,h2,h3,p,button,summary,a.btn")
    .evaluateAll((elements) =>
      elements
        .filter((element) => {
          if (element.classList.contains("sr-only")) return false;
          return (
            element.getBoundingClientRect().width > 0 &&
            element.scrollWidth > element.clientWidth + 2
          );
        })
        .map((element) => element.textContent?.trim()),
    );
  expect(overflowing).toEqual([]);
});

for (const density of [2, 3]) {
  test(`${density}× screens receive native high-resolution previews`, async ({
    browser,
  }) => {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      deviceScaleFactor: density,
      reducedMotion: "reduce",
    });
    try {
      const page = await context.newPage();
      await page.route("https://api.github.com/repos/**", (route) =>
        route.fulfill({ status: 503, body: "Fixture" }),
      );
      await page.goto(site);
      const image = page.locator("#preview-image");
      await expect
        .poll(() =>
          image.evaluate((element: HTMLImageElement) => element.currentSrc),
        )
        .toMatch(/app-conversations@2x\.webp$/);
      const original = page.getByRole("link", { name: "Enlarge app preview" });
      await expect(original).toHaveAttribute(
        "href",
        "screenshots/app-conversations@2x.webp",
      );
      await original.click();
      await expect(page.locator(".image-viewer img")).toHaveAttribute(
        "src",
        /app-conversations@2x\.webp$/,
      );
      expect(
        await page
          .locator(".image-viewer img")
          .evaluate((element: HTMLImageElement) => element.naturalWidth),
      ).toBe(2560);
      await page.keyboard.press("Escape");
      await page.locator("#theme-barcelona").click();
      await expect(page.locator("#theme-barcelona")).toHaveAttribute(
        "aria-selected",
        "true",
      );
      await expect
        .poll(() =>
          page
            .locator("#customization-image")
            .evaluate((element: HTMLImageElement) => element.currentSrc),
        )
        .toMatch(
          density === 3 ? /hero-barcelona@2x\.webp$/ : /hero-barcelona\.webp$/,
        );
      await expect(page.locator(".customization-link")).toHaveAttribute(
        "href",
        "screenshots/hero-barcelona@2x.webp",
      );
    } finally {
      await context.close();
    }
  });
}

test("mobile previews avoid unnecessarily fetching desktop-size images", async ({
  browser,
}) => {
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    reducedMotion: "reduce",
  });
  try {
    const page = await context.newPage();
    await page.route("https://api.github.com/repos/**", (route) =>
      route.fulfill({ status: 503, body: "Fixture" }),
    );
    await page.goto(site);
    await expect
      .poll(() =>
        page
          .locator("#preview-image")
          .evaluate((element: HTMLImageElement) => element.currentSrc),
      )
      .toMatch(/\/app-conversations\.webp$/);
    await expect(page.locator("#preview-image")).toHaveAttribute(
      "srcset",
      /1280w[\s\S]*2560w/,
    );
  } finally {
    await context.close();
  }
});
