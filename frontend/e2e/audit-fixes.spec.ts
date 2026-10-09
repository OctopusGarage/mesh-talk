import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { test, expect } from "./tauri-mock";
import { enterChat, CHANNEL } from "./helpers/session";

test.use({ viewport: { width: 760, height: 520 } });

test("member removal and dialog close have visible, usable focus targets", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  const remove = dialog.getByRole("button", { name: "Remove" }).first();
  await remove.focus();
  await expect(remove).toHaveCSS("opacity", "1");

  for (const target of [
    remove,
    dialog.getByRole("button", { name: "Close" }),
  ]) {
    const box = await target.boundingBox();
    expect(box).not.toBeNull();
    // Chromium can report a CSS 44px edge as 43.999969 after compositing.
    expect(Math.round(box!.width)).toBeGreaterThanOrEqual(44);
    expect(Math.round(box!.height)).toBeGreaterThanOrEqual(44);
  }
});

test("device-linking fields are named and failed clipboard writes are announced", async ({
  page,
}) => {
  await enterChat(page);
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible())) {
    await page.getByTestId("sidebar-overflow").click();
  }
  await linkAction.click();
  const dialog = page.getByTestId("link-device-dialog");
  await expect(dialog.getByRole("combobox", { name: "Device" })).toBeVisible();
  await expect(
    dialog.getByRole("textbox", { name: "pairing code" }),
  ).toBeVisible();

  await dialog.getByTestId("link-show-code").click();
  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async () => {
          throw new Error("Clipboard denied");
        },
      },
    });
  });
  await dialog.locator('button[title="Copy"]').click();
  await expect(dialog.getByRole("alert")).toContainText("Couldn't copy");
});

test("diagnostics copy controls report clipboard failure", async ({ page }) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  const dialog = page.getByTestId("diagnostics-dialog");
  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async () => {
          throw new Error("Clipboard denied");
        },
      },
    });
  });

  await dialog.locator('button[title="Copy"]').first().click();
  await expect(dialog.getByRole("alert")).toContainText("Couldn't copy");

  await page.getByTestId("diagnostics-tab-help").click();
  await dialog.locator('button[title="Copy"]').first().click();
  await expect(dialog.getByRole("alert")).toContainText("Couldn't copy");
});

test("closing during code generation cleans up the late pairing session", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const tauri = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = tauri.invoke.bind(tauri);
    let release: (() => void) | undefined;
    let starts = 0;
    let stops = 0;
    (window as unknown as Record<string, unknown>).__linkProbe = {
      release: () => release?.(),
      counts: () => ({ starts, stops }),
    };
    tauri.invoke = (cmd, args) => {
      if (cmd === "start_linking") {
        starts += 1;
        return new Promise((resolve, reject) => {
          release = () => void invoke(cmd, args).then(resolve, reject);
        });
      }
      if (cmd === "stop_linking") stops += 1;
      return invoke(cmd, args);
    };
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  const dialog = page.getByTestId("link-device-dialog");
  const show = dialog.getByTestId("link-show-code");
  await show.click();
  await expect(show).toBeDisabled();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __linkProbe: { counts: () => { starts: number } };
            }
          ).__linkProbe.counts().starts,
      ),
    )
    .toBe(1);
  await dialog.getByRole("button", { name: "Close" }).click();
  await page.evaluate(() =>
    (
      window as unknown as { __linkProbe: { release: () => void } }
    ).__linkProbe.release(),
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __linkProbe: { counts: () => { stops: number } };
            }
          ).__linkProbe.counts().stops,
      ),
    )
    .toBe(1);
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible())) {
    await page.getByTestId("sidebar-overflow").click();
  }
  await linkAction.click();
  await expect(page.getByTestId("pairing-code")).toHaveCount(0);
  await expect(page.getByTestId("link-show-code")).toBeEnabled();
});

test("a new pairing code waits for the previous session to stop", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const tauri = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = tauri.invoke.bind(tauri);
    let release: (() => void) | undefined;
    let starts = 0;
    (window as unknown as Record<string, unknown>).__stopProbe = {
      release: () => release?.(),
      starts: () => starts,
    };
    tauri.invoke = (cmd, args) => {
      if (cmd === "start_linking") starts += 1;
      if (cmd === "stop_linking") {
        return new Promise((resolve, reject) => {
          release = () => void invoke(cmd, args).then(resolve, reject);
        });
      }
      return invoke(cmd, args);
    };
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  await page.getByTestId("link-show-code").click();
  await expect(page.getByTestId("pairing-code")).toBeVisible();
  await page
    .getByTestId("link-device-dialog")
    .getByRole("button", { name: "Close" })
    .click();
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await linkAction.click();
  await page.getByTestId("link-show-code").click();
  await expect(page.getByTestId("link-show-code")).toBeDisabled();
  expect(
    await page.evaluate(() =>
      (
        window as unknown as { __stopProbe: { starts: () => number } }
      ).__stopProbe.starts(),
    ),
  ).toBe(1);
  await page.evaluate(() =>
    (
      window as unknown as { __stopProbe: { release: () => void } }
    ).__stopProbe.release(),
  );
  await expect(page.getByTestId("pairing-code")).toBeVisible();
  expect(
    await page.evaluate(() =>
      (
        window as unknown as { __stopProbe: { starts: () => number } }
      ).__stopProbe.starts(),
    ),
  ).toBe(2);
});
