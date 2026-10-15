import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("no network uses the attention state in connection details", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockSetNetworkInterfaces: (interfaces: string[]) => void;
      }
    ).__mockSetNetworkInterfaces([]);
  });
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  const status = page.getByTestId("diagnostics-status");
  await expect(status).toContainText("No network connection");
  await expect(status.getByText("No network connection")).toHaveClass(
    /text-attention/,
  );
});

test("connection details distinguish load failure from an empty network", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("diag_network_info");
  });
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  const dialog = page.getByTestId("diagnostics-dialog");
  await expect(page.getByTestId("diagnostics-status")).toContainText(
    "Ready to find people",
  );
  await expect(dialog.getByRole("alert")).toContainText(
    "Couldn’t load network details",
  );
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  await expect(dialog.getByText("Discovery port")).toBeVisible();
});

test("a late network load cannot replace details after reopening", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as {
      __rejectOldNetwork?: (error: Error) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const invoke = win.__TAURI_INTERNALS__.invoke.bind(win.__TAURI_INTERNALS__);
    let first = true;
    win.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "diag_network_info" && first) {
        first = false;
        return new Promise((_, reject) => {
          win.__rejectOldNetwork = reject;
        });
      }
      return invoke(cmd, args);
    };
  });
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  const dialog = page.getByTestId("diagnostics-dialog");
  await page.waitForFunction(
    () =>
      typeof (window as unknown as { __rejectOldNetwork?: unknown })
        .__rejectOldNetwork === "function",
  );
  await dialog.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  await expect(dialog.getByText("Discovery port")).toBeVisible();
  await page.evaluate(async () => {
    (
      window as unknown as { __rejectOldNetwork: (error: Error) => void }
    ).__rejectOldNetwork(new Error("stale network failure"));
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
  await expect(dialog.getByRole("alert")).toHaveCount(0);
});

test("a failed announce is visible on the current connection tab", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  const dialog = page.getByTestId("diagnostics-dialog");
  await page.getByTestId("diagnostics-tab-peers").click();
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("rescan_peers");
  });
  await dialog.getByRole("button", { name: "Announce now" }).click();
  await expect(page.getByTestId("diagnostics-action-error")).toContainText(
    "Action failed",
  );
  await page
    .getByTestId("diagnostics-action-error")
    .getByRole("button", { name: "Dismiss" })
    .click();
  await expect(page.getByTestId("diagnostics-action-error")).toHaveCount(0);
});

test("manual endpoint discovery retries after failure and submits a trimmed address", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-connection");
  await page.getByTestId("diagnostics-tab-help").click();
  await page.getByTestId("diagnostics-offline-connect").click();

  const dialog = page.getByTestId("offline-connect-dialog");
  const address = dialog.getByRole("textbox", {
    name: "Device IP address or hostname",
  });
  const findDevice = dialog.getByRole("button", { name: "Find device" });
  await expect(findDevice).toBeDisabled();
  await address.fill("  100.101.102.103:47474  ");
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("probe_peer_endpoint");
  });
  await findDevice.click();
  await expect(dialog.getByRole("alert")).toContainText("network unreachable");
  await expect(dialog.getByRole("status")).toHaveCount(0);

  await findDevice.click();
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  await expect(dialog.getByRole("status")).toContainText(
    "Discovery query sent",
  );
  const probedEndpoints = await page.evaluate(
    () =>
      (window as unknown as { __mockProbedEndpoints: string[] })
        .__mockProbedEndpoints,
  );
  expect(probedEndpoints).toEqual(["100.101.102.103:47474"]);

  await address.fill("100.101.102.104");
  await expect(dialog.getByRole("status")).toHaveCount(0);
});
