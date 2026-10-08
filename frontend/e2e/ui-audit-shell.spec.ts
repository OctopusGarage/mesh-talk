import { expect, test } from "./tauri-mock";
import { enterChat, BOB, CAROL } from "./helpers/session";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

test("app shell stays fixed while inner panes retain their scroll areas", async ({
  page,
}) => {
  await enterChat(page);

  const layout = await page.evaluate(() => {
    const html = getComputedStyle(document.documentElement);
    const body = getComputedStyle(document.body);
    const nav = document.querySelector('[data-testid="sidebar"] nav');
    if (!nav) throw new Error("Sidebar navigation missing");
    return {
      htmlOverflow: html.overflowY,
      bodyOverflow: body.overflowY,
      htmlOverscroll: html.overscrollBehaviorY,
      bodyOverscroll: body.overscrollBehaviorY,
      navOverflow: getComputedStyle(nav).overflowY,
    };
  });

  expect(layout).toEqual({
    htmlOverflow: "hidden",
    bodyOverflow: "hidden",
    htmlOverscroll: "none",
    bodyOverscroll: "none",
    navOverflow: "auto",
  });
});

test("shell and sidebar meet baseline layout and interaction invariants", async ({
  page,
}) => {
  await enterChat(page);

  await expectNoHorizontalOverflow(page, "chat shell");
  await expect(page.getByTestId("sidebar-action-search")).toHaveText(
    "Search messages",
  );
  await expect(page.getByTestId("sidebar-action-files")).toHaveText(
    "Received files",
  );
  await expect(page.getByTestId("lan-online-count")).toContainText("online");
  await expect(page.getByTestId("lan-online-count")).toHaveAttribute(
    "aria-label",
    /person online on the LAN/,
  );
  await expect(page.getByTestId("sidebar-nav-connection")).toHaveText(
    "Connection",
  );
  await expect(page.getByTestId("sidebar-nav-settings")).toHaveText("Settings");
  await page.getByTestId("sidebar-nav-connection").click();
  await expect(page.getByTestId("diagnostics-dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByTestId("sidebar-nav-settings").click();
  await expect(page.getByTestId("settings-dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expectMinTargetSize(
    page.getByTestId("sidebar-action-search"),
    32,
    "sidebar search",
  );
  await expectMinTargetSize(
    page.getByTestId("sidebar-action-files"),
    32,
    "received files",
  );
  await expectElementsWithin(
    page,
    '[data-testid^="conversation-row-"]',
    '[data-testid="sidebar"]',
  );

  await page.getByTestId(`conversation-row-${BOB.account}`).hover();
  await expectVisibleFocus(page, page.getByTestId("sidebar-overflow"));
  await expectMinTargetSize(
    page.getByTestId(`conversation-actions-${BOB.account}`),
    32,
    "conversation actions",
  );
  await page.getByTestId(`conversation-actions-${BOB.account}`).click();
  for (const id of [
    "open-profile",
    "sidebar-overflow",
    `conversation-rename-${BOB.account}`,
    `conversation-pin-${BOB.account}`,
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 32, id);
  }

  await expect(
    page.getByTestId(`conversation-rename-${BOB.account}`),
  ).toHaveAttribute("aria-label", /bob/i);
  await expect(
    page.getByTestId(`conversation-pin-${BOB.account}`),
  ).toHaveAttribute("aria-label", /bob/i);
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-rename-${BOB.account}`),
  );
  await page.getByTestId(`conversation-actions-${BOB.account}`).click();
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-pin-${BOB.account}`),
  );
  await page.keyboard.press("Escape");
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-row-${BOB.account}`),
  );

  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(
    page.getByTestId(`conversation-row-${BOB.account}`),
  ).toContainText("thanks! glad to be here");
});

test("stranded prompt dismiss target meets interaction invariants", async ({
  page,
}) => {
  await page.clock.install();
  await enterChat(page);

  await page.evaluate(
    ({ bob, carol }) => {
      const setPresence = (
        window as unknown as {
          __mockSetPresence?: (
            next: Record<
              string,
              { online: boolean; last_seen_secs: number | null }
            >,
          ) => void;
        }
      ).__mockSetPresence;
      if (!setPresence) throw new Error("__mockSetPresence is not installed");
      setPresence({
        [bob]: { online: false, last_seen_secs: 120 },
        [carol]: { online: false, last_seen_secs: 120 },
      });
    },
    { bob: BOB.account, carol: CAROL.account },
  );

  await page.clock.runFor(25_000);

  await expectMinTargetSize(
    page.getByTestId("stranded-dismiss"),
    32,
    "stranded-dismiss",
  );
  await expect(page.getByTestId("stranded-dismiss")).toHaveAttribute(
    "aria-label",
    "Dismiss",
  );
  await expectVisibleFocus(page, page.getByTestId("stranded-dismiss"));
});

for (const language of ["en", "es", "ja", "zh-Hans", "zh-Hant", "yue"]) {
  test(`sidebar actions fit in ${language}`, async ({ page }) => {
    await page.addInitScript(
      (nextLanguage) => localStorage.setItem("mesh-talk-lang", nextLanguage),
      language,
    );
    await enterChat(page);
    const bounds = await page.evaluate(() => {
      const sidebar = document.querySelector('[data-testid="sidebar"]');
      const search = document.querySelector(
        '[data-testid="sidebar-action-search"]',
      );
      const searchLabel = search?.querySelector("span");
      const files = document.querySelector(
        '[data-testid="sidebar-action-files"]',
      );
      if (!sidebar || !search || !searchLabel || !files)
        throw new Error("Sidebar action missing");
      return {
        sidebar: sidebar.getBoundingClientRect().right,
        search: search.getBoundingClientRect().right,
        searchLabelFits: searchLabel.scrollWidth <= searchLabel.clientWidth + 1,
        files: files.getBoundingClientRect().right,
      };
    });
    expect(
      bounds.search,
      `${language}: search exceeds sidebar`,
    ).toBeLessThanOrEqual(bounds.sidebar + 0.5);
    expect(
      bounds.files,
      `${language}: files exceeds sidebar`,
    ).toBeLessThanOrEqual(bounds.sidebar + 0.5);
    expect(bounds.searchLabelFits, `${language}: search label is clipped`).toBe(
      true,
    );
  });
}
