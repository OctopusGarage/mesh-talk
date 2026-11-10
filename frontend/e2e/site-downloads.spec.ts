import { servePublicSite } from "./helpers/public-site";
import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

let site: string;
let hosted: Awaited<ReturnType<typeof servePublicSite>>;
test.beforeAll(async () => {
  hosted = await servePublicSite();
  site = hosted.url;
});
test.afterAll(async () => {
  await hosted?.close();
});
const api =
  "https://api.github.com/repos/OctopusGarage/mesh-talk/releases/latest";
const root = "https://github.com/OctopusGarage/mesh-talk/releases";
function release() {
  const tag = "v9.8.7";
  return {
    tag_name: tag,
    draft: false,
    prerelease: false,
    html_url: `${root}/tag/${tag}`,
    assets: [
      "macos_arm64",
      "macos_x86_64",
      "windows_arm64",
      "windows_x86_64",
      "linux_aarch64",
      "linux_x86_64",
    ].map((target) => {
      const name = `mesh-talk_${tag}_${target}.zip`;
      return {
        name,
        size: 12345678,
        state: "uploaded",
        browser_download_url: `${root}/download/${tag}/${name}`,
      };
    }),
  };
}
async function setup(page: Page, os: string, architecture?: string) {
  await page.addInitScript(
    ({ os, architecture }) => {
      Object.defineProperty(navigator, "userAgent", {
        value:
          os === "macOS"
            ? "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"
            : os === "iOS"
              ? "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)"
              : `Mozilla/5.0 (${os})`,
      });
      Object.defineProperty(navigator, "userAgentData", {
        value: {
          platform: os,
          mobile: os === "iOS",
          getHighEntropyValues: async () => ({ architecture, bitness: "64" }),
        },
      });
      Object.defineProperty(navigator, "clipboard", {
        value: {
          writeText: async (text: string) => {
            document.documentElement.dataset.copied = text;
          },
        },
      });
    },
    { os, architecture },
  );
  await page.route(api, (route) =>
    route.fulfill({
      json: release(),
      headers: { "access-control-allow-origin": "*" },
    }),
  );
  await page.route(`${root}/download/**`, (route) =>
    route.fulfill({
      body: "fixture archive",
      contentType: "application/zip",
      headers: {
        "content-disposition": 'attachment; filename="mesh-talk.zip"',
      },
    }),
  );
}

// WebKit does not emit a download for intercepted attachment responses, even
// without application code. Assert its outgoing exact asset request here; real
// native attachment events are asserted in Chromium and Firefox.
function observePackageRequest(page: Page): Promise<string> {
  return page.context().browser()?.browserType().name() === "webkit"
    ? page
        .waitForRequest((request) =>
          request.url().startsWith(`${root}/download/`),
        )
        .then((request) => request.url())
    : page.waitForEvent("download").then((download) => download.url());
}

test("Mac chip choice leads to a direct archive download and first-run dialog", async ({
  page,
}) => {
  await setup(page, "macOS");
  await page.goto(site);
  await expect(
    page.getByRole("tab", { name: "macOS", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#release-status")).toContainText("Stable 9.8.7");
  const hero = page.locator("[data-download-primary]");
  await expect(hero).toHaveText(/Download for macOS/);
  await hero.click();
  const dialog = page.getByRole("dialog", {
    name: "Open Mesh-Talk on your Mac",
  });
  await expect(dialog).toBeVisible();
  const download = observePackageRequest(page);
  await dialog.getByRole("link", { name: /Apple silicon/ }).click();
  expect(await download).toContain("mesh-talk_v9.8.7_macos_arm64.zip");
  await expect(dialog).toContainText("Unzip");
  await expect(dialog).toContainText("/Applications/mesh-talk.app");
  await dialog.getByRole("button", { name: "Copy command" }).click();
  await expect(page.locator("html")).toHaveAttribute(
    "data-copied",
    '/usr/bin/xattr -dr com.apple.quarantine "/Applications/mesh-talk.app"',
  );
  await expect(dialog.locator(".copy-feedback")).toHaveText("Command copied.");
  const audit = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(audit.violations).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(hero).toBeFocused();
});

for (const [os, architecture, platform, target] of [
  ["Windows", "x86", "Windows", "windows_x86_64"],
  ["Windows", "arm", "Windows", "windows_arm64"],
  ["Linux", "arm", "Linux", "linux_aarch64"],
  ["macOS", "arm", "macOS", "macos_arm64"],
]) {
  test(`detects ${os} ${architecture} and downloads the matching desktop package`, async ({
    page,
  }) => {
    await setup(page, os, architecture);
    await page.goto(site);
    await expect(
      page.getByRole("tab", { name: platform, exact: true }),
    ).toHaveAttribute("aria-selected", "true");
    const hero = page.locator("[data-download-primary]");
    await expect(hero).toHaveAttribute("href", new RegExp(`${target}\\.zip$`));
    const download = observePackageRequest(page);
    await hero.click();
    expect(await download).toContain(target);
    await expect(page.locator(".install-dialog")).toBeVisible();
    await page.getByRole("button", { name: "Got it", exact: true }).click();
    await expect(hero).toBeFocused();
  });
}

test("users can override detection and select another architecture", async ({
  page,
}) => {
  await setup(page, "macOS", "arm");
  await page.goto(site);
  await page.getByRole("tab", { name: "Windows", exact: true }).click();
  const panel = page.getByRole("tabpanel", { name: "Windows", exact: true });
  const link = panel.getByRole("link", { name: /Download for ARM64/ });
  await expect(link).toHaveAttribute("href", /windows_arm64\.zip$/);
  const download = observePackageRequest(page);
  await link.click();
  expect(await download).toContain("windows_arm64");
  await expect(
    page.getByRole("dialog", { name: "Install Mesh-Talk on Windows" }),
  ).toBeVisible();
});

test("API failure offers a retry and preserves the release-page fallback", async ({
  page,
}) => {
  await setup(page, "Windows", "x86");
  await page.route(api, (route) =>
    route.fulfill({
      status: 403,
      json: { message: "rate limited" },
      headers: { "access-control-allow-origin": "*" },
    }),
  );
  await page.goto(site);
  await expect(page.locator("#release-status")).toContainText("couldn’t load");
  await expect(
    page
      .locator("#download")
      .getByRole("link", { name: "Browse Stable release", exact: true }),
  ).toHaveAttribute("href", `${root}/latest`);
  await page.unroute(api);
  await page.route(api, (route) =>
    route.fulfill({
      json: release(),
      headers: { "access-control-allow-origin": "*" },
    }),
  );
  await page.getByRole("button", { name: "Try again", exact: true }).click();
  await expect(page.locator("[data-download-primary]")).toHaveAttribute(
    "href",
    /windows_x86_64\.zip$/,
  );
});

for (const bad of ["prerelease", "foreign-url", "duplicate", "missing"]) {
  test(`release metadata cannot choose a ${bad} package`, async ({ page }) => {
    await setup(page, "macOS", "arm");
    const data = release();
    if (bad === "prerelease") data.prerelease = true;
    if (bad === "foreign-url")
      data.assets[0].browser_download_url = "https://example.com/installer.zip";
    if (bad === "duplicate") data.assets.push(data.assets[0]);
    if (bad === "missing")
      data.assets = data.assets.filter((a) => !a.name.includes("macos_arm64"));
    await page.route(api, (route) =>
      route.fulfill({
        json: data,
        headers: { "access-control-allow-origin": "*" },
      }),
    );
    await page.goto(site);
    await expect(page.locator("#release-status")).toHaveText(
      /Stable 9\.8\.7|couldn’t load/,
    );
    await expect(
      page.locator("#platform-macos a[data-package][href*='macos_arm64']"),
    ).toHaveCount(0);
    await expect(page.locator("[data-download-primary]")).not.toHaveAttribute(
      "href",
      /macos_arm64\.zip$/,
    );
  });
}

test("mobile browsers keep explicit desktop platform choices", async ({
  page,
}) => {
  await setup(page, "iOS");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(site);
  await expect(page.locator("[data-download-primary]")).toHaveText(
    /Choose a desktop download/,
  );
  await expect(page.locator("#release-status")).toContainText("9.8.7");
  await page.getByRole("tab", { name: "Linux", exact: true }).click();
  const panel = page.getByRole("tabpanel", { name: "Linux", exact: true });
  await panel
    .getByRole("button", { name: "How to install", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "Install Mesh-Talk on Linux",
  });
  await expect(dialog).toBeVisible();
  expect(await dialog.evaluate((e) => e.scrollWidth <= e.clientWidth + 1)).toBe(
    true,
  );
  await dialog.getByRole("button", { name: "Got it", exact: true }).click();
});

test("clipboard denial leaves the command selectable with manual copy guidance", async ({
  page,
}) => {
  await setup(page, "macOS");
  await page.goto(site);
  await expect(page.locator("#release-status")).toContainText("Stable 9.8.7");
  await page.evaluate(() => {
    navigator.clipboard.writeText = async () => {
      throw new Error("Denied");
    };
  });
  await page.locator("[data-download-primary]").click();
  const dialog = page.getByRole("dialog", {
    name: "Open Mesh-Talk on your Mac",
  });
  await dialog.getByRole("button", { name: "Copy command" }).click();
  await expect(dialog.locator(".copy-feedback")).toContainText(
    "command is selected",
  );
  expect(
    await page.evaluate(() => window.getSelection()?.toString()),
  ).toContain("/Applications/mesh-talk.app");
});

test("release lookup times out and remains retryable", async ({ page }) => {
  await setup(page, "macOS");
  await page.route(api, () => {});
  await page.clock.install();
  await page.goto(site);
  await expect(page.locator("#release-status")).toContainText("Loading");
  await page.clock.fastForward(8100);
  await expect(page.locator("#release-status")).toContainText("couldn’t load");
  await expect(
    page.getByRole("button", { name: "Try again", exact: true }),
  ).toBeVisible();
});

test("manual platform and architecture choices update the hero and survive reload", async ({
  page,
}) => {
  await setup(page, "macOS", "arm");
  await page.goto(site);
  await expect(page.locator("#release-status")).toContainText("Stable 9.8.7");
  await page.getByRole("tab", { name: "Windows", exact: true }).click();
  await expect(page.locator("[data-download-primary]")).toHaveText(
    "Download for Windows",
  );
  await page.locator("[data-download-primary]").click();
  const dialog = page.getByRole("dialog", {
    name: "Install Mesh-Talk on Windows",
  });
  const download = observePackageRequest(page);
  await dialog.getByRole("link", { name: /Download for ARM64/ }).click();
  await download;
  await expect(dialog.locator("[data-download-info]")).toContainText("ARM64");
  await expect(
    dialog.getByRole("link", { name: "Download again" }),
  ).toHaveAttribute("href", /windows_arm64\.zip$/);
  expect(
    await page.evaluate(() =>
      document
        .querySelector(".install-dialog")
        ?.contains(document.activeElement),
    ),
  ).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-download-primary]")).toHaveAttribute(
    "href",
    /windows_arm64\.zip$/,
  );
  await page.reload();
  await page.getByRole("tab", { name: "Windows", exact: true }).click();
  await expect(page.locator("[data-download-primary]")).toHaveAttribute(
    "href",
    /windows_arm64\.zip$/,
  );
});

test("late clipboard responses cannot update a reopened installation guide", async ({
  page,
}) => {
  await setup(page, "macOS");
  await page.goto(site);
  await expect(page.locator("#release-status")).toContainText("Stable 9.8.7");
  await page.evaluate(() => {
    navigator.clipboard.writeText = () =>
      new Promise<void>((resolve) => {
        (window as Window & { finishCopy?: () => void }).finishCopy = resolve;
      });
  });
  await page.locator("[data-download-primary]").click();
  const dialog = page.getByRole("dialog", {
    name: "Open Mesh-Talk on your Mac",
  });
  await dialog.getByRole("button", { name: "Copy command" }).click();
  await page.keyboard.press("Escape");
  await page.locator("[data-download-primary]").click();
  await page.evaluate(() =>
    (window as Window & { finishCopy?: () => void }).finishCopy?.(),
  );
  await expect(dialog.locator(".copy-feedback")).toHaveText("");
  await expect(
    dialog.getByRole("button", { name: "Copy command" }),
  ).toBeEnabled();
});
