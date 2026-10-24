import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { test, expect } from "./tauri-mock";
import { enterChat, CHANNEL } from "./helpers/session";

test.use({ viewport: { width: 760, height: 520 } });

for (const viewport of [
  { width: 760, height: 520 },
  { width: 390, height: 720 },
]) {
  test(`pairing connection guidance and recovery stay usable at ${viewport.width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await page.evaluate(() => {
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args?: unknown,
              options?: unknown,
            ) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const invoke = internals.invoke.bind(internals);
      internals.invoke = (command, args, options) =>
        command === "link_device"
          ? Promise.reject({
              kind: "link-timeout",
              message: "pairing response timed out (probe diagnostic)",
            })
          : invoke(command, args, options);
    });
    const action = page.getByTestId("sidebar-action-link");
    if (!(await action.isVisible()))
      await page.getByTestId("sidebar-overflow").click();
    await action.click();
    const dialog = page.getByTestId("link-device-dialog");
    await expect(
      dialog.getByText(/shared network or a reachable VPN/),
    ).toBeAttached();
    await dialog
      .getByRole("combobox", { name: "Device" })
      .selectOption("device_bob_2222");
    await dialog
      .getByRole("textbox", { name: "pairing code" })
      .fill("0123 4567 89ab cdef 0123 4567 89ab cdef");
    await expect(
      dialog.getByText("Keep this device’s account", { exact: true }),
    ).toBeAttached();
    await expect(dialog.getByTestId("link-account-change")).toContainText(
      "bob",
    );
    await expect(dialog.getByTestId("link-account-change")).toContainText(
      "keeps its account",
    );
    await expect(dialog.getByTestId("link-adopt-account")).toBeDisabled();
    await dialog.locator("#link-account-consent").check();
    await dialog.getByTestId("link-adopt-account").click();
    const feedback = dialog.locator("#link-device-feedback");
    await expect(feedback).toContainText("didn’t answer in time");
    await feedback.scrollIntoViewIfNeeded();
    await expect(feedback).toBeInViewport();
    await expect(feedback.locator("details")).not.toHaveAttribute("open");
    await feedback.getByText("Technical details").click();
    await expect(feedback.getByText(/probe diagnostic/)).toBeVisible();
    await feedback.getByText("Technical details").click();
    await dialog.getByTestId("link-refresh-devices").click();
    await expect(dialog.getByRole("status")).toContainText(
      "Discovery requested",
    );
    await expect(dialog.getByTestId("link-adopt-account")).toBeEnabled();
    expect(
      await dialog.evaluate(
        (element) => element.scrollWidth <= element.clientWidth + 1,
      ),
    ).toBe(true);
    await feedback.scrollIntoViewIfNeeded();
    await dialog.screenshot({
      path: testInfo.outputPath(`pairing-${viewport.width}.png`),
    });
  });
}

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

test("device linking offers user devices but excludes post office nodes", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=post-office");
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible())) {
    await page.getByTestId("sidebar-overflow").click();
  }
  await linkAction.click();
  const devices = page
    .getByTestId("link-device-dialog")
    .getByRole("combobox", { name: "Device" });
  await expect(devices.getByRole("option", { name: /bob/ })).toBeAttached();
  await expect(devices.getByRole("option", { name: /relay node/ })).toHaveCount(
    0,
  );
});

test("device linking excludes mailbox-only devices before accepting a code", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=pairing-routes");
  const action = page.getByTestId("sidebar-action-link");
  if (!(await action.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await action.click();
  const dialog = page.getByTestId("link-device-dialog");
  const devices = dialog.getByRole("combobox", { name: "Device" });
  await expect(devices.locator("option[value=device_bob_2222]")).toHaveCount(1);
  await expect(devices.locator("option[value=device_carol_3333]")).toHaveCount(
    0,
  );
  await expect(dialog.locator("#link-device-route-hint")).toContainText(
    "cannot be selected",
  );
  await dialog
    .getByRole("textbox", { name: "pairing code" })
    .fill("0123456789abcdef0123456789abcdef");
  const link = dialog.getByTestId("link-adopt-account");
  await expect(link).toBeDisabled();
  await devices.selectOption("device_bob_2222");
  await expect(link).toBeDisabled();
  await dialog.locator("#link-account-consent").check();
  await expect(link).toBeEnabled();
});

test("linked-device history choice is local and survives reopening", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  const dialog = page.getByTestId("link-device-dialog");
  const history = dialog.getByRole("switch", {
    name: "Message history",
  });
  const profile = dialog.getByRole("switch", { name: "Account profile" });
  const attachments = dialog.getByRole("switch", { name: "Attachments" });
  const appearance = dialog.getByRole("switch", {
    name: "Preferences and libraries",
  });
  const contacts = dialog.getByRole("switch", { name: "Contacts" });
  await expect(history).toHaveAttribute("aria-checked", "true");
  await expect(profile).toHaveAttribute("aria-checked", "true");
  await expect(attachments).toHaveAttribute("aria-checked", "false");
  await expect(appearance).toHaveAttribute("aria-checked", "false");
  await expect(contacts).toHaveAttribute("aria-checked", "false");
  await history.click();
  await profile.click();
  await attachments.click();
  await appearance.click();
  await contacts.click();
  await expect(history).toHaveAttribute("aria-checked", "false");
  await expect(profile).toHaveAttribute("aria-checked", "false");
  await expect(attachments).toHaveAttribute("aria-checked", "true");
  await expect(appearance).toHaveAttribute("aria-checked", "true");
  await expect(contacts).toHaveAttribute("aria-checked", "true");

  await dialog.getByRole("button", { name: "Close" }).click();
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await linkAction.click();
  await expect(history).toHaveAttribute("aria-checked", "false");
  await expect(profile).toHaveAttribute("aria-checked", "false");
  await expect(attachments).toHaveAttribute("aria-checked", "true");
  await expect(appearance).toHaveAttribute("aria-checked", "true");
  await expect(contacts).toHaveAttribute("aria-checked", "true");
});

test("linked-device status shows pending attachments and failures", async ({
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
    tauri.invoke = (cmd, args) =>
      cmd === "get_device_sync_status"
        ? Promise.resolve([
            {
              device_id: "abcd1234".repeat(4),
              phase: "error",
              pending_attachments: 2,
              error: "source offline",
            },
          ])
        : invoke(cmd, args);
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  const dialog = page.getByTestId("link-device-dialog");
  await expect(dialog).toContainText("2 attachments pending");
  await expect(dialog.getByRole("alert")).toContainText("source offline");
});

test("failed sync choice leaves the device policy unchanged", async ({
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
    tauri.invoke = (cmd, args) =>
      cmd === "set_device_sync_category"
        ? Promise.reject(new Error("disk write failed"))
        : invoke(cmd, args);
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  const dialog = page.getByTestId("link-device-dialog");
  const history = dialog.getByRole("switch", {
    name: "Message history",
  });
  await expect(history).toHaveAttribute("aria-checked", "true");
  await history.click();
  await expect(dialog.getByRole("alert")).toContainText("disk write failed");
  await expect(history).toHaveAttribute("aria-checked", "true");
});

test("late sync choice cannot replace policy after the dialog reopens", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as {
      __resolveSyncSave?: (policy: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const invoke = win.__TAURI_INTERNALS__.invoke.bind(win.__TAURI_INTERNALS__);
    win.__TAURI_INTERNALS__.invoke = (cmd, args) =>
      cmd === "set_device_sync_category"
        ? new Promise((resolve) => {
            win.__resolveSyncSave = resolve;
          })
        : invoke(cmd, args);
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-link").click();
  const dialog = page.getByTestId("link-device-dialog");
  const history = dialog.getByRole("switch", { name: "Message history" });
  await expect(history).toHaveAttribute("aria-checked", "true");
  await history.click();
  await page.waitForFunction(
    () =>
      typeof (window as unknown as { __resolveSyncSave?: unknown })
        .__resolveSyncSave === "function",
  );
  await dialog.getByRole("button", { name: "Close" }).click();
  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await linkAction.click();
  await expect(history).toHaveAttribute("aria-checked", "true");
  await page.evaluate(() => {
    (
      window as unknown as { __resolveSyncSave: (policy: unknown) => void }
    ).__resolveSyncSave({
      history: false,
      attachments: false,
      profile: true,
      appearance: false,
      contacts: false,
    });
  });
  await expect(history).toBeEnabled();
  await expect(history).toHaveAttribute("aria-checked", "true");
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

for (const theme of [null, "oled"] as const) {
  test(`pairing publishes ${theme ? "saved appearance only" : "no fresh appearance defaults"}`, async ({
    page,
  }) => {
    await page.addInitScript((savedTheme) => {
      if (savedTheme) localStorage.setItem("mesh-talk-theme", savedTheme);
    }, theme);
    await enterChat(page);
    await page.evaluate(() => {
      const win = window as typeof window & {
        __appearanceWrites: { key: unknown; value: unknown }[];
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      win.__appearanceWrites = [];
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = (command, args) => {
        if (
          ["seed_account_preference", "set_account_preference"].includes(
            command,
          ) &&
          args.category === "appearance"
        ) {
          win.__appearanceWrites.push({ key: args.key, value: args.value });
        }
        return original(command, args);
      };
    });
    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-action-link").click();
    await page.getByTestId("link-show-code").click();
    await expect(page.getByTestId("pairing-code")).toBeVisible();
    const writes = await page.evaluate(
      () =>
        (
          window as typeof window & {
            __appearanceWrites: { key: unknown; value: unknown }[];
          }
        ).__appearanceWrites,
    );
    expect(writes).toEqual(theme ? [{ key: "theme", value: theme }] : []);
  });
}

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

test("pairing preference seeds preserve existing values and reject retired accounts", async ({
  page,
}) => {
  await enterChat(page);
  const result = await page.evaluate(async () => {
    const invoke = (
      window as typeof window & {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__.invoke;
    const scope = {
      owner: "u_self",
      expectedAccount: "acc_self_aaaa1111",
      category: "appearance",
      key: "theme",
    };
    const first = await invoke("seed_account_preference", {
      ...scope,
      value: "oled",
    });
    const second = await invoke("seed_account_preference", {
      ...scope,
      value: "light",
    });
    let rejected = false;
    try {
      await invoke("seed_account_preference", {
        ...scope,
        expectedAccount: "retired-account",
        key: "accent",
        value: "blue",
      });
    } catch {
      rejected = true;
    }
    return {
      first,
      second,
      rejected,
      rows: await invoke("get_account_preferences", scope),
    };
  });
  expect(result.first).toMatchObject({ key: "theme", value: "oled" });
  expect(result.second).toEqual(result.first);
  expect(result.rejected).toBe(true);
  expect(result.rows).toEqual([result.first]);
});
