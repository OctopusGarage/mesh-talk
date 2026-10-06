import { expect, test } from "./tauri-mock";
import { enterChat, BOB, CAROL } from "./helpers/session";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

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
  for (const id of [
    "open-profile",
    "sidebar-overflow",
    `conversation-rename-${BOB.account}`,
    `conversation-pin-${BOB.account}`,
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 32, id);
  }

  await expectVisibleFocus(page, page.getByTestId("sidebar-overflow"));
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
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-pin-${BOB.account}`),
  );
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-row-${BOB.account}`),
  );
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
      const files = document.querySelector(
        '[data-testid="sidebar-action-files"]',
      );
      if (!sidebar || !search || !files)
        throw new Error("Sidebar action missing");
      return {
        sidebar: sidebar.getBoundingClientRect().right,
        search: search.getBoundingClientRect().right,
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
  });
}
