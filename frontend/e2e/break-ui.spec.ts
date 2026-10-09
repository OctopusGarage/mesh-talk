import type { Page } from "@playwright/test";
import { expect, test } from "./tauri-mock";
import {
  expectDialogFitsViewport,
  expectElementsWithin,
  expectNoHorizontalOverflow,
} from "./helpers/ui-audit";

const BOB = "acc_bob_bbbb2222";

async function enter(page: Page, mode: string, username = "tester") {
  await page.goto(`/?data=${mode}`);
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill(username);
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
}

async function dialogOverflow(page: Page, testId: string) {
  return page.getByTestId(testId).evaluate((dialog) => {
    const right = dialog.getBoundingClientRect().right;
    return {
      scrollWidth: dialog.scrollWidth,
      clientWidth: dialog.clientWidth,
      offenders: Array.from(dialog.querySelectorAll<HTMLElement>("*"))
        .filter((el) => el.getBoundingClientRect().right > right + 1)
        .slice(0, 6)
        .map((el) => ({
          tag: el.tagName,
          className: el.className,
          text: el.textContent?.slice(0, 40),
          right: el.getBoundingClientRect().right,
        })),
    };
  });
}

test("a maximum-length username stays inside identity surfaces", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  const username = "device_operator_release_lab_" + "x".repeat(22);
  expect(username.length).toBe(50);
  await enter(page, "worst", username);
  await expectNoHorizontalOverflow(page, "long own username");
  await page.getByTestId("open-profile").click();
  await expect(page.getByTestId("profile-dialog")).toBeVisible();
  await expectDialogFitsViewport(page, "profile-dialog");
  const profileOverflow = await dialogOverflow(page, "profile-dialog");
  expect(
    profileOverflow.scrollWidth,
    JSON.stringify(profileOverflow),
  ).toBeLessThanOrEqual(profileOverflow.clientWidth + 4);
  await expectNoHorizontalOverflow(page, "long own profile");
});

for (const viewport of [
  { width: 760, height: 520 },
  { width: 1280, height: 800 },
  { width: 2560, height: 1440 },
]) {
  test(`worst-case names and history fit at ${viewport.width}×${viewport.height}`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await enter(page, "worst");
    await expectNoHorizontalOverflow(page, "worst-case roster");
    expect(
      await page
        .getByTestId("conversation-nav")
        .evaluate((nav) => nav.scrollWidth <= nav.clientWidth + 1),
    ).toBe(true);
    await page.getByTestId(`conversation-row-${BOB}`).click();
    await expect(page.getByRole("log")).toBeVisible();
    await expect(page.getByTestId("message-bubble").first()).toBeVisible();
    await expectElementsWithin(
      page,
      '[data-testid="conversation-history-trigger"], [data-testid="verify-trigger"]',
      '[data-testid="conversation-header"]',
    );
    await expectElementsWithin(page, '[data-testid="composer-input"]', "main");
    await expectNoHorizontalOverflow(page, "worst-case conversation");
    const metrics = await page.getByRole("log").evaluate((log) => ({
      renderedRows: log.querySelectorAll('[data-testid="message-bubble"]')
        .length,
      scrollable: log.scrollHeight > log.clientHeight,
      overflows: log.scrollWidth > log.clientWidth + 1,
    }));
    expect(metrics.scrollable).toBe(true);
    expect(metrics.overflows).toBe(false);
    expect(metrics.renderedRows).toBeLessThan(100);
  });
}

test("worst-case content remains usable at 200% browser zoom", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await enter(page, "worst");
  await page.evaluate(() => {
    document.body.style.zoom = "2";
  });
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(page.getByTestId("message-bubble").first()).toBeVisible();
  await expectElementsWithin(
    page,
    '[data-testid="conversation-history-trigger"], [data-testid="verify-trigger"]',
    '[data-testid="conversation-header"]',
  );
  await expectElementsWithin(page, '[data-testid="composer-input"]', "main");
  await expectNoHorizontalOverflow(page, "200% zoom");
});

test("a long conversation name does not overlap its row action", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  const row = page.getByTestId(`conversation-row-${BOB}`);
  await row.click();
  await expect(row.locator(".sidebar-row-time")).toBeVisible();
  await row.hover();
  await expect(page.getByTestId(`conversation-actions-${BOB}`)).toBeVisible();
  await expect(row.locator(".sidebar-row-time")).toHaveCSS("opacity", "0");
});

test("long channel names and authors stay inside the header and log", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await expect(page.getByRole("log")).toBeVisible();
  await expectElementsWithin(
    page,
    '[data-testid="conversation-history-trigger"]',
    '[data-testid="conversation-header"]',
  );
  await expectNoHorizontalOverflow(page, "long channel names");
  expect(
    await page
      .getByRole("log")
      .evaluate((log) => log.scrollWidth <= log.clientWidth + 1),
  ).toBe(true);
});

test("CJK and emoji search results stay usable with dense history", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  await page.getByTestId("sidebar-action-search").click();
  await page.getByTestId("search-input").fill("家族");
  await expect(page.getByTestId("search-result").first()).toBeVisible();
  const dialog = page.getByTestId("search-dialog");
  expect(
    await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth + 4),
  ).toBe(true);
  await expectNoHorizontalOverflow(page, "Unicode search");
  await page.getByTestId("search-result").first().click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
});

test("a long device name stays inside diagnostics", async ({ page }) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  await page.getByTestId("sidebar-nav-connection").click();
  await page.getByTestId("diagnostics-tab-peers").click();
  const dialog = page.getByTestId("diagnostics-dialog");
  await expect(dialog).toBeVisible();
  await expectDialogFitsViewport(page, "diagnostics-dialog");
  const diagnosticOverflow = await dialogOverflow(page, "diagnostics-dialog");
  expect(
    diagnosticOverflow.scrollWidth,
    JSON.stringify(diagnosticOverflow),
  ).toBeLessThanOrEqual(diagnosticOverflow.clientWidth + 4);
  await expectNoHorizontalOverflow(page, "long device diagnostics");
});

test("a thousand conversations remain scrollable and selectable", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "huge");
  const nav = page.getByTestId("conversation-nav");
  await expect(nav).toBeVisible();
  expect(await nav.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(
    true,
  );
  expect(
    await nav.locator('[data-testid^="conversation-row-"]').count(),
  ).toBeLessThan(80);
  await nav.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await page.getByTestId("conversation-row-acc_bulk_999").click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="composer-input"]', "main");
  await expectNoHorizontalOverflow(page, "large roster");
  await page.getByTestId("conversation-row-acc_bulk_999").focus();
  await page.keyboard.press("ArrowDown");
  await expect(
    page.getByTestId("conversation-row-chan_team_dddd4444"),
  ).toBeFocused();
});

test("a thousand pinned conversations stay bounded and keyboard reachable", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "pinned-huge");
  const nav = page.getByTestId("conversation-nav");
  expect(
    await nav.locator('[data-testid^="conversation-row-"]').count(),
  ).toBeLessThan(80);
  await nav.evaluate((el) => {
    el.scrollTop = 1000 * 52;
  });
  const lastPinned = page.getByTestId("conversation-row-acc_bulk_999");
  await lastPinned.focus();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId(`conversation-row-${BOB}`)).toBeFocused();
  await expectNoHorizontalOverflow(page, "large pinned roster");
});

test("a thousand channels stay bounded and keyboard reachable", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "channels-huge");
  const nav = page.getByTestId("conversation-nav");
  expect(
    await nav.locator('[data-testid^="conversation-row-"]').count(),
  ).toBeLessThan(80);
  await page.getByTestId("conversation-row-acc_carol_cccc3333").focus();
  await page.keyboard.press("ArrowDown");
  await expect(
    page.getByTestId("conversation-row-chan_team_dddd4444"),
  ).toBeFocused();
  await nav.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await page.getByTestId("conversation-row-chan_bulk_999").click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expectNoHorizontalOverflow(page, "large channel roster");
});

test("keyboard navigation crosses a virtualized conversation boundary", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "huge");
  const nav = page.getByTestId("conversation-nav");
  const lastRendered = nav.locator("[data-virtual-index]").last();
  const index = Number(await lastRendered.getAttribute("data-virtual-index"));
  await lastRendered.focus();
  await page.keyboard.press("ArrowDown");
  await expect
    .poll(() =>
      page.evaluate(
        () => (document.activeElement as HTMLElement)?.dataset.virtualIndex,
      ),
    )
    .toBe(String(index + 1));
  await expect(
    nav.locator(`[data-virtual-index="${index + 1}"]`),
  ).toBeVisible();
});

for (const mode of ["empty", "offline"]) {
  test(`${mode} roster keeps navigation and empty states usable`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 760, height: 520 });
    await enter(page, mode);
    await expectNoHorizontalOverflow(page, `${mode} roster`);
    await expect(page.getByTestId("sidebar-nav-settings")).toBeVisible();
    if (mode === "offline") {
      await page.getByTestId(`conversation-row-${BOB}`).click();
      await expect(page.getByRole("log")).toBeVisible();
    } else {
      await expect(page.getByTestId(`conversation-row-${BOB}`)).toHaveCount(0);
    }
  });
}

test("no-peer state offers a direct connection path with announce feedback", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "empty");
  await page.getByTestId("empty-connect").click();
  const dialog = page.getByTestId("offline-connect-dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("On the same network")).toBeVisible();
  await expectDialogFitsViewport(page, "offline-connect-dialog");
  await page.getByTestId("connect-announce").click();
  await expect(dialog.getByRole("status")).toContainText("announced itself");
  await dialog.getByRole("button", { name: "Connect without Wi-Fi" }).click();
  await expect(dialog.getByText("One device shares a hotspot")).toBeVisible();
  await expectNoHorizontalOverflow(page, "connection guide");
});

test("changed safety number stays legible in a narrow conversation", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "changed");
  await page.getByTestId(`conversation-row-${BOB}`).click();
  const trigger = page.getByTestId("verify-trigger");
  await expect(trigger).toHaveAttribute("data-trust", "changed");
  await expect(trigger.getByText("Re-verify")).toBeVisible();
  await expectElementsWithin(
    page,
    '[data-testid="verify-trigger"]',
    '[data-testid="conversation-header"]',
  );
  await expectNoHorizontalOverflow(page, "changed identity warning");
});

test("rapid incoming messages and conversation switches stay usable", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  await page.getByTestId(`conversation-row-${BOB}`).click();
  const log = page.getByRole("log");
  await expect(page.getByTestId("message-bubble").first()).toBeVisible();
  // The 500-message backlog must finish measuring before this test exercises
  // followOutput. A mounted bubble alone does not mean Virtuoso reached the end.
  await expect
    .poll(() =>
      log.evaluate((element) => element.scrollHeight - element.clientHeight),
    )
    .toBeGreaterThan(200);
  await log.press("End");
  await expect
    .poll(() =>
      log.evaluate(
        (element) =>
          element.scrollHeight - element.clientHeight - element.scrollTop,
      ),
    )
    .toBeLessThan(48);
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockInject: (conv: string, text: string, who: string) => void;
      __mockEmit: (event: string, payload: unknown) => void;
    };
    for (let i = 0; i < 30; i++) {
      const text = `Rapid update ${i + 1} — 家族 👩🏽‍💻`;
      w.__mockInject("acc:acc_bob_bbbb2222", text, "device_bob_2222");
      w.__mockEmit("dm-received", {
        from: "device_bob_2222",
        from_name: "Aleksandra Wiśniewska-Kowalczyk",
        text,
        reply_to: null,
      });
    }
  });
  await expect(
    page.getByRole("log").getByText("Rapid update 30 — 家族 👩🏽‍💻"),
  ).toBeVisible();
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await expect(page.getByTestId("conversation-empty")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(
    page.getByRole("log").getByText("Rapid update 30 — 家族 👩🏽‍💻"),
  ).toBeVisible();
  await expectNoHorizontalOverflow(page, "rapid updates");
});

test("long connection and security errors stay inside their dialogs", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enter(page, "worst");
  const error =
    "connection-refused-while-reading-encrypted-device-metadata-from-a-disconnected-peer.example.test";
  await page.evaluate((message) => {
    (
      window as unknown as {
        __mockFailNext: (command: string, message: string) => void;
      }
    ).__mockFailNext("diag_network_info", message);
  }, error);
  await page.getByTestId("sidebar-nav-connection").click();
  await expect(
    page.getByTestId("diagnostics-dialog").getByRole("alert"),
  ).toBeVisible();
  await expectDialogFitsViewport(page, "diagnostics-dialog");
  expect(
    await page
      .getByTestId("diagnostics-dialog")
      .evaluate((dialog) => dialog.scrollWidth <= dialog.clientWidth + 1),
  ).toBe(true);
  await expectNoHorizontalOverflow(page, "connection error");
  await page.keyboard.press("Escape");

  await page.getByTestId(`conversation-row-${BOB}`).click();
  await page.evaluate((message) => {
    (
      window as unknown as {
        __mockFailNext: (command: string, message: string) => void;
      }
    ).__mockFailNext("safety_number", message);
  }, error);
  await page.getByTestId("verify-trigger").click();
  await expect(
    page.getByTestId("verify-dialog").getByText(error),
  ).toBeVisible();
  await expectDialogFitsViewport(page, "verify-dialog");
  expect(
    await page
      .getByTestId("verify-dialog")
      .getByText(error)
      .evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  ).toBe(true);
  const verifyOverflow = await page
    .getByTestId("verify-dialog")
    .evaluate((dialog) => ({
      scrollWidth: dialog.scrollWidth,
      clientWidth: dialog.clientWidth,
    }));
  // Radix's stable scrollbar gutter contributes a few px to scrollWidth.
  expect(verifyOverflow.scrollWidth).toBeLessThanOrEqual(
    verifyOverflow.clientWidth + 4,
  );
  await expectNoHorizontalOverflow(page, "security error");
});
