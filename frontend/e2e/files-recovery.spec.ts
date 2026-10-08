import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("received files does not guess the download folder after a failed read", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("get_app_settings");
  });
  await page.getByTestId("sidebar-action-files").click();
  const files = page.getByTestId("files-tray");
  await expect(page.getByTestId("files-folder-error")).toContainText(
    "Couldn’t load the download folder",
  );
  await expect(files.getByText("Folder unavailable")).toBeVisible();
  await page
    .getByTestId("files-folder-error")
    .getByRole("button", { name: "Retry" })
    .click();
  await expect(page.getByTestId("files-folder-error")).toHaveCount(0);
  await expect(files.getByText("/home/tester/Downloads")).toBeVisible();
});
