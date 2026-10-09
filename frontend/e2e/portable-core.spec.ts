import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import type { Page } from "@playwright/test";
import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, BOB, CHANNEL } from "./helpers/session";
import { seedMarketPacks } from "./helpers/packs";
import {
  expectDialogFitsViewport,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
} from "./helpers/ui-audit";

test("city avatar thumbnails load after scrolling the gallery", async ({
  page,
}) => {
  await seedMarketPacks(page, ["cities"]);
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  const gallery = page.getByTestId("avatar-gallery");
  await expect(gallery.locator("img")).toHaveCount(36);

  await gallery.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  const abuDhabi = gallery
    .getByRole("button", { name: "Abu Dhabi" })
    .locator("img");
  await expect(abuDhabi).toHaveAttribute("src", /^data:image\/jpeg;base64,/);
  await expect
    .poll(() =>
      abuDhabi.evaluate((image) => (image as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(1);
  for (const index of [28, 29, 30, 31, 32, 33, 34, 35]) {
    const thumbnail = gallery.locator("img").nth(index);
    await expect
      .poll(() =>
        thumbnail.evaluate((image) => (image as HTMLImageElement).naturalWidth),
      )
      .toBeGreaterThan(1);
  }
});

test("portable authentication rejects the wrong password", async ({ page }) => {
  await page.goto("/");
  // A rejected backend response, delivered through the same IPC boundary as Tauri.
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (cmd, args) =>
      cmd === "login" && args.password === "incorrect"
        ? Promise.reject(new Error("Invalid username or password"))
        : invoke(cmd, args);
  });
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("incorrect");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
  await expect(
    page.getByText("Invalid username or password", { exact: true }),
  ).toBeVisible();
});

test("portable registration sign in and sign out", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/");
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    let registered: string | undefined;
    internals.invoke = async (cmd, args) => {
      if (cmd === "login" && args.username !== registered)
        throw new Error("Account has not been registered");
      const result = await invoke(cmd, args);
      if (cmd === "register" && (result as { success: boolean }).success)
        registered = String(args.username);
      return result;
    };
  });
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("login-tab-signin")).toHaveAttribute(
    "data-state",
    "active",
  );
  await expect(
    page.getByText("Account created — you can sign in now.", { exact: true }),
  ).toBeVisible();
  await expect(page.getByTestId("login-password")).toHaveValue("");
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByTestId("sidebar-own-name")).toHaveText("tester");
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-sign-out").click();
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
  expect(
    await page
      .getByTestId("login-form")
      .evaluate((form) =>
        Number(getComputedStyle(form.closest(".bg-card")!).opacity),
      ),
    "sign-out must reveal the sign-in form immediately",
  ).toBe(1);
});

async function settings(page: Page) {
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("settings-dialog")).toBeVisible();
}

async function dialogKeyboardContained(page: Page, id: string) {
  for (const key of [
    "Tab",
    "Tab",
    "Tab",
    "Tab",
    "Shift+Tab",
    "Shift+Tab",
    "Shift+Tab",
    "Shift+Tab",
  ]) {
    await page.keyboard.press(key);
    expect(
      await page
        .getByTestId(id)
        .evaluate((el) => el.contains(document.activeElement)),
      `${id} lost focus after ${key}`,
    ).toBe(true);
  }
}

test("light theme persists across browser reload and sign in", async ({
  page,
}) => {
  await enterChat(page);
  await settings(page);
  await page.getByTestId("theme-light").click();
  await expect(page.locator("html")).not.toHaveClass(/dark/);
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-theme")),
  ).toBe("light");
  await page.reload();
  await expect(page.locator("html")).not.toHaveClass(/dark/);
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await settings(page);
  await expect(page.getByTestId("theme-light")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.locator("html")).not.toHaveClass(/dark/);
});

async function boundedShell(page: Page) {
  await expectNoHorizontalOverflow(page, "portable chat");
  for (const id of [
    "chat-shell",
    "conversation-header",
    "composer-input",
    "composer-send",
    "open-profile",
  ]) {
    const target = page.getByTestId(id);
    await expect(target).toBeVisible();
    const box = await target.boundingBox();
    expect(box).not.toBeNull();
    const viewport = page.viewportSize()!;
    expect(box!.width).toBeGreaterThan(0);
    expect(box!.height).toBeGreaterThan(0);
    expect(box!.x).toBeGreaterThanOrEqual(-1);
    expect(box!.y).toBeGreaterThanOrEqual(-1);
    expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width + 1);
    expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height + 1);
  }
  await expect(
    page.getByText("Something went wrong", { exact: true }),
  ).toHaveCount(0);
}

async function inject(page: Page, channel: boolean, text: string) {
  await page.evaluate(
    ({ channel, text }) => {
      const w = window as unknown as {
        __mockInject: (conv: string, text: string, who: string) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      w.__mockInject(
        channel ? "ch:chan_team_dddd4444" : "acc:acc_bob_bbbb2222",
        text,
        "device_bob_2222",
      );
      w.__mockEmit(channel ? "channel-message" : "dm-received", {
        from: "device_bob_2222",
        from_name: "bob",
        channel_id: "chan_team_dddd4444",
        channel_name: "team",
        text,
        reply_to: null,
      });
    },
    { channel, text },
  );
}

for (const viewport of [
  { width: 760, height: 520 },
  { width: 1040, height: 720 },
]) {
  test.describe(`${viewport.width}x${viewport.height} browser-only core`, () => {
    test.use({ viewport });
    test("shell and avatar expose bounded interactive targets", async ({
      page,
    }) => {
      await enterChat(page);
      await openBobDm(page);
      await boundedShell(page);
      await expectMinTargetSize(page.getByTestId("open-profile"));
      await page.getByTestId("composer-input").fill("bounded send");
      await page.getByTestId("composer-send").click();
      await expect(
        page.getByRole("log").getByText("bounded send", { exact: true }),
      ).toBeVisible();
    });
    test("DM sends and renders incoming event delivery", async ({ page }) => {
      await enterChat(page);
      await openBobDm(page);
      await page.getByTestId("composer-input").fill("portable outbound DM");
      await page.getByTestId("composer-send").click();
      await expect(
        page
          .getByRole("log")
          .getByText("portable outbound DM", { exact: true }),
      ).toBeVisible();
      await inject(page, false, "portable incoming DM");
      await expect(
        page
          .getByRole("log")
          .getByText("portable incoming DM", { exact: true }),
      ).toBeVisible();
      await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
      await page.getByTestId(`conversation-row-${BOB.account}`).click();
      await expect(
        page
          .getByRole("log")
          .getByText("portable outbound DM", { exact: true }),
      ).toBeVisible();
      await expect(
        page
          .getByRole("log")
          .getByText("portable incoming DM", { exact: true }),
      ).toBeVisible();
      await boundedShell(page);
    });
    test("group conversation sends and renders incoming events", async ({
      page,
    }) => {
      await enterChat(page);
      await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
      await expect(
        page.getByRole("log").getByText("channel kickoff", { exact: true }),
      ).toBeVisible();
      await page.getByTestId("composer-input").fill("portable group outbound");
      await page.getByTestId("composer-send").click();
      await expect(
        page
          .getByRole("log")
          .getByText("portable group outbound", { exact: true }),
      ).toBeVisible();
      await inject(page, true, "portable group incoming");
      await expect(
        page
          .getByRole("log")
          .getByText("portable group incoming", { exact: true }),
      ).toBeVisible();
      await boundedShell(page);
    });
    test("profile dialog fits and preserves keyboard focus while renaming", async ({
      page,
    }) => {
      await enterChat(page);
      await page.getByTestId("open-profile").click();
      await expect(page.getByTestId("profile-dialog")).toBeVisible();
      await expectDialogFitsViewport(page, "profile-dialog");
      await page.getByTestId("profile-name").click();
      const input = page.getByTestId("profile-name-input");
      await expect(input).toBeFocused();
      await input.fill("老王 🐻");
      await page.getByTestId("profile-name-save").click();
      await expect(page.getByTestId("sidebar-own-name")).toHaveText("老王 🐻");
      await dialogKeyboardContained(page, "profile-dialog");
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("profile-dialog")).toHaveCount(0);
      await page.getByTestId("open-profile").click();
      await expect(page.getByTestId("profile-dialog")).toBeVisible();
    });
    test("settings switches light dark and Chinese English within bounds", async ({
      page,
    }) => {
      await enterChat(page);
      await settings(page);
      await expectDialogFitsViewport(page, "settings-dialog");
      await dialogKeyboardContained(page, "settings-dialog");
      await page.getByTestId("theme-light").click();
      await expect(page.locator("html")).not.toHaveClass(/dark/);
      await page
        .getByTestId("settings-language-select")
        .selectOption("zh-Hans");
      await expect(page.getByText("私信", { exact: true })).toBeVisible();
      await page.keyboard.press("Escape");
      await openBobDm(page);
      await boundedShell(page);
      await page.getByTestId("composer-input").fill("浅色中文消息 👋");
      await page.getByTestId("composer-send").click();
      await expect(
        page.getByRole("log").getByText("浅色中文消息 👋", { exact: true }),
      ).toBeVisible();
      await page.getByTestId("open-profile").click();
      await expectDialogFitsViewport(page, "profile-dialog");
      await page.keyboard.press("Escape");
      await settings(page);
      await page.getByTestId("theme-dark").click();
      await expect(page.locator("html")).toHaveClass(/dark/);
      await page.getByTestId("settings-language-select").selectOption("en");
      await expect(
        page.getByText("Direct messages", { exact: true }),
      ).toBeVisible();
      await page.keyboard.press("Escape");
      await openBobDm(page);
      await boundedShell(page);
    });
    test("Unicode multiline and unbroken wide messages stay in the log", async ({
      page,
    }) => {
      await enterChat(page);
      await openBobDm(page);
      const text = "你好 👋 café\nsecond line\n" + "界".repeat(140);
      const input = page.getByTestId("composer-input");
      await input.fill(text);
      await expect(input).toHaveValue(text);
      await page.getByTestId("composer-send").click();
      await expect(input).toHaveValue("");
      const last = page.getByTestId("message-bubble").last();
      await expect(last).toContainText(text);
      await expect(
        last.locator("span.whitespace-pre-wrap").filter({ hasText: "你好" }),
      ).toHaveCSS("white-space", "pre-wrap");
      const geometry = await last.evaluate((el) => {
        const bubble = el.querySelector("[data-context-menu]")!;
        const log = document.querySelector('[role="log"]')!;
        const b = bubble.getBoundingClientRect(),
          l = log.getBoundingClientRect();
        return {
          left: b.left - l.left,
          right: b.right - l.right,
          width: b.width,
          logWidth: l.width,
          overflow: bubble.scrollWidth - bubble.clientWidth,
        };
      });
      expect(geometry.left).toBeGreaterThanOrEqual(-1);
      expect(geometry.right).toBeLessThanOrEqual(1);
      expect(geometry.width).toBeLessThanOrEqual(geometry.logWidth * 0.75);
      expect(geometry.overflow).toBeLessThanOrEqual(1);
      await boundedShell(page);
    });
  });
}

if (process.env.EVAL_TIER === "extended")
  test("@extended palette locale resize and repeated dialog DOM retention smoke", async ({
    page,
  }) => {
    await seedMarketPacks(page, ["barcelona", "argentina", "messi"]);
    await enterChat(page);
    await openBobDm(page);
    const before = await page.locator("*").count();
    for (const theme of [
      "oled",
      "barcelona",
      "argentina",
      "messi",
      "light",
      "dark",
    ]) {
      await settings(page);
      await page.getByTestId(`theme-${theme}`).click();
      await expect(page.getByTestId(`theme-${theme}`)).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect
        .poll(() =>
          page.evaluate(() => localStorage.getItem("mesh-talk-theme")),
        )
        .toBe(theme);
      await page
        .getByTestId("settings-language-select")
        .selectOption("zh-Hans");
      await expect(page.getByText("私信", { exact: true })).toBeVisible();
      await page.keyboard.press("Escape");
      await page.setViewportSize({ width: 760, height: 520 });
      await boundedShell(page);
      await page.setViewportSize({ width: 1040, height: 720 });
      await boundedShell(page);
      await page.getByTestId("open-profile").click();
      await expectDialogFitsViewport(page, "profile-dialog");
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("profile-dialog")).toHaveCount(0);
    }
    // Portable DOM retention smoke, not a claim about native RSS/heap collection.
    expect(await page.locator("*").count()).toBeLessThanOrEqual(before + 20);
    await expect(page.getByRole("dialog")).toHaveCount(0);
  });
