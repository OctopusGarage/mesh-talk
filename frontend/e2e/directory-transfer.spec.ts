import { test, expect } from "./tauri-mock";
import {
  enterChat,
  openBobDm,
  revealComposerTools,
  BOB,
} from "./helpers/session";

test("folder picker sends its path as an attachment", async ({ page }) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const win = window as unknown as Window & {
      __folderSend: { path: string; media: boolean } | null;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__folderSend = null;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "plugin:dialog|open")
        return Promise.resolve("/tmp/family photos");
      if (cmd === "owner_enqueue_file")
        win.__folderSend = {
          path: String(args.path),
          media: Boolean(args.media),
        };
      return original(cmd, args);
    };
  });
  await revealComposerTools(page);
  await page.getByTestId("composer-attach-directory").click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __folderSend: unknown }).__folderSend,
      ),
    )
    .toEqual({ path: "/tmp/family photos", media: false });
});

test("received folder stays disabled until ready and extracts through save-to-dir", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(({ account, device }) => {
    const win = window as unknown as Window & {
      __folderReady: boolean;
      __folderSave: { fileConv: string; dir: string } | null;
      __mockEmit: (name: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__folderReady = false;
    win.__folderSave = null;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "file_statuses")
        return (args.fileConvs as string[]).map((file_conv) => ({
          file_conv,
          done: win.__folderReady ? 2 : 1,
          total: 2,
          ready: win.__folderReady,
        }));
      if (cmd === "save_file_to_dir") {
        win.__folderSave = {
          fileConv: String(args.fileConv),
          dir: String(args.dir),
        };
        return "/home/tester/Downloads/family photos";
      }
      const result = await original(cmd, args);
      if (cmd === "owner_account_history" && args.account === account)
        return [
          ...(result as unknown[]),
          {
            id: "received-folder",
            from_me: false,
            who: device,
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              file_conv: "fc_folder",
              name: "family photos.tar",
              size: 2048,
              mime: "application/x-mesh-talk-directory-tar",
              media: false,
            },
          },
        ];
      return result;
    };
    win.__mockEmit("file-received", {
      conv: account,
      from: device,
      name: "family photos.tar",
      size: 2048,
      mime: "application/x-mesh-talk-directory-tar",
      file_conv: "fc_folder",
      media: false,
    });
  }, BOB);
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "family photos" });
  await expect(
    bubble.getByText("family photos", { exact: true }),
  ).toBeVisible();
  await expect(bubble.getByRole("button", { name: "Save" })).toBeDisabled();
  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await page.evaluate(() => {
    (window as unknown as { __folderReady: boolean }).__folderReady = true;
  });
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeEnabled();
  await tray.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __folderSave: unknown }).__folderSave,
      ),
    )
    .toEqual({ fileConv: "fc_folder", dir: "/home/tester/Downloads" });
  await expect(tray.getByRole("button", { name: "Reveal" })).toBeVisible();
});
