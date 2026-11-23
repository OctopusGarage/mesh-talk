import { test, expect } from "./tauri-mock";
import { waitForChatShell, enterChat } from "./helpers/session";

// Exercises the real React listeners through the Tauri event fixture. Native
// menu rendering and window activation require a separate desktop check.
for (const [action, dialog] of [
  ["settings", "settings-dialog"],
  ["diagnostics", "diagnostics-dialog"],
  ["about", "about-dialog"],
  ["help", "help-dialog"],
] as const) {
  test(`tray ${action} opens its existing dialog`, async ({ page }) => {
    await enterChat(page);
    await page.evaluate((payload) => {
      const emit = (
        window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        }
      ).__mockEmit;
      emit("tray-navigate", payload);
    }, action);
    await expect(page.getByTestId(dialog)).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId(dialog)).not.toBeVisible();
    await waitForChatShell(page);
  });
}

test("an unknown tray destination does not open a dialog", async ({ page }) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("tray-navigate", "unknown");
  });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await waitForChatShell(page);
});

test("switching tray destinations replaces the open dialog", async ({
  page,
}) => {
  await enterChat(page);
  for (const destination of ["settings", "help", "diagnostics", "about"]) {
    await page.evaluate((payload) => {
      (
        window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        }
      ).__mockEmit("tray-navigate", payload);
    }, destination);
    await expect(page.getByTestId(`${destination}-dialog`)).toBeVisible();
    await expect(page.getByRole("dialog", { includeHidden: true })).toHaveCount(
      1,
    );
  }
});

test("About remains reachable through the sidebar menu", async ({ page }) => {
  await enterChat(page);
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-about").click();
  await expect(page.getByTestId("about-dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("about-dialog")).not.toBeVisible();
});

test("tray diagnostics releases resource sampling when closed", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as {
      __mockEmit: (event: string, payload: unknown) => void;
      __resourceLeaseCalls: string[];
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const invoke = win.__TAURI_INTERNALS__.invoke.bind(win.__TAURI_INTERNALS__);
    win.__resourceLeaseCalls = [];
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "env_info") {
        const env = await invoke(cmd, args);
        return {
          ...(env as Record<string, unknown>),
          desktop_ready_ms: 125,
          initial_interface_ms: 390,
        };
      }
      if (cmd === "start_host_resource_sampling") {
        win.__resourceLeaseCalls.push("start");
        return 7;
      }
      if (cmd === "host_resource_sample") {
        win.__resourceLeaseCalls.push(`sample:${args.ticket}`);
        return {
          processes: [
            {
              pid: 1,
              birth: "host",
              cpu_ms: 0,
              resident_bytes: 20_000_000,
              disk_write_bytes: null,
              io_write_bytes: null,
            },
          ],
          sampled_at_ms: 100,
          bounded: false,
          authenticated_channels: 2,
          sent_frame_bytes: 120,
          received_frame_bytes: 64,
        };
      }
      if (cmd === "stop_host_resource_sampling") {
        win.__resourceLeaseCalls.push(`stop:${args.ticket}`);
        return;
      }
      return invoke(cmd, args);
    };
    win.__mockEmit("tray-navigate", "diagnostics");
  });
  const dialog = page.getByTestId("diagnostics-dialog");
  const start = dialog.getByRole("button", { name: "Start sampling" });
  await expect(start).toBeVisible();
  await expect(dialog.getByText("Desktop host ready in 125 ms")).toBeVisible();
  await expect(
    dialog.getByText("Initial interface prepared in 390 ms"),
  ).toBeVisible();
  const calls = () =>
    page.evaluate(
      () =>
        (window as unknown as { __resourceLeaseCalls: string[] })
          .__resourceLeaseCalls,
    );
  expect(await calls()).toEqual([]);
  await start.click();
  await expect(
    dialog.locator("dl").getByText("20 MB", { exact: true }).first(),
  ).toBeVisible();
  await expect(dialog.getByText("120 byte")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect.poll(calls).toEqual(["start", "sample:7", "stop:7"]);
});
