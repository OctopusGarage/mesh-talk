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
  await page.getByTestId("sidebar-nav-connection").click();
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
  await page.getByTestId("sidebar-nav-connection").click();
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

test("a failed announce is visible on the current connection tab", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("sidebar-nav-connection").click();
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
