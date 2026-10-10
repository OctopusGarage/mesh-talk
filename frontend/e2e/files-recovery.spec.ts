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
  await expect(files.getByTestId("files-open-folder")).toBeDisabled();
  await page
    .getByTestId("files-folder-error")
    .getByRole("button", { name: "Retry" })
    .click();
  await expect(page.getByTestId("files-folder-error")).toHaveCount(0);
  await expect(files.getByText("/home/tester/Downloads")).toBeVisible();
  await expect(files.getByTestId("files-open-folder")).toBeEnabled();
});

test("received files opens the configured or system download folder", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as Window & {
      __openedFolderPaths: string[];
      __folderOpenFails: boolean;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__openedFolderPaths = [];
    win.__folderOpenFails = false;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "plugin:opener|open_path") {
        if (win.__folderOpenFails)
          return Promise.reject(new Error("file manager unavailable"));
        win.__openedFolderPaths.push(String(args.path));
        return Promise.resolve(null);
      }
      if (cmd === "default_download_dir")
        return Promise.resolve("/home/tester/System Downloads");
      return original(cmd, args);
    };
  });

  await page.getByTestId("sidebar-action-files").click();
  const files = page.getByTestId("files-tray");
  const openFolder = files.getByTestId("files-open-folder");
  await expect(openFolder).toBeEnabled();
  await openFolder.click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __openedFolderPaths: string[] })
            .__openedFolderPaths,
      ),
    )
    .toEqual(["/home/tester/Downloads"]);

  await page.evaluate(async () => {
    await (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__.invoke("set_app_settings", {
      settings: { download_dir: "" },
    });
  });
  await page.keyboard.press("Escape");
  await page.getByTestId("sidebar-action-files").click();
  await expect(openFolder).toBeEnabled();
  await openFolder.click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __openedFolderPaths: string[] })
            .__openedFolderPaths,
      ),
    )
    .toEqual(["/home/tester/Downloads", "/home/tester/System Downloads"]);

  await page.evaluate(() => {
    (window as unknown as { __folderOpenFails: boolean }).__folderOpenFails =
      true;
  });
  await openFolder.click();
  await expect(files.getByTestId("files-open-folder-error")).toContainText(
    "file manager unavailable",
  );
  await expect(openFolder).toBeEnabled();
});
