import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, BOB } from "./helpers/session";

test("an incomplete attachment shows progress and cannot be saved from chat or tray", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(({ account, device }) => {
    const win = window as unknown as Window & {
      __transferReady: boolean;
      __transferSaves: number;
      __mockEmit: (name: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__transferReady = false;
    win.__transferSaves = 0;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "file_statuses") {
        return (args.fileConvs as string[]).map((file_conv) => ({
          file_conv,
          done: win.__transferReady ? 4 : 2,
          total: 4,
          ready: win.__transferReady,
        }));
      }
      if (cmd === "save_file" || cmd === "save_file_to_dir")
        win.__transferSaves++;
      const result = await original(cmd, args);
      if (cmd === "owner_account_history" && args.account === account)
        return [
          ...(result as unknown[]),
          {
            id: "received-attachment",
            from_me: false,
            who: device,
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              file_conv: "fc_incomplete_attachment",
              name: "report.pdf",
              size: 4096,
              mime: "application/pdf",
              media: false,
            },
          },
        ];
      return result;
    };
    win.__mockEmit("file-received", {
      conv: account,
      from: device,
      name: "report.pdf",
      size: 4096,
      mime: "application/pdf",
      file_conv: "fc_incomplete_attachment",
      media: false,
    });
  }, BOB);
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "report.pdf" });
  await expect(bubble.getByRole("progressbar")).toHaveAttribute(
    "aria-valuenow",
    "50",
  );
  await expect(bubble.getByRole("button", { name: "Save" })).toBeDisabled();

  await page.getByTestId("conversation-history-trigger").click();
  const history = page.getByTestId("conversation-history-dialog");
  await history.getByRole("tab", { name: /Files/ }).click();
  const historyFile = history
    .getByTestId("history-file-item")
    .filter({ hasText: "report.pdf" });
  await expect(historyFile.getByRole("progressbar")).toHaveAttribute(
    "aria-valuenow",
    "50",
  );
  await expect(
    historyFile.getByRole("button", { name: "Save" }),
  ).toBeDisabled();
  await page.keyboard.press("Escape");

  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(tray.getByRole("progressbar")).toHaveAttribute(
    "aria-valuenow",
    "50",
  );
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await expect(tray.getByRole("button", { name: "Save as…" })).toBeDisabled();
  expect(
    await page.evaluate(
      () => (window as unknown as { __transferSaves: number }).__transferSaves,
    ),
  ).toBe(0);

  await page.evaluate(() => {
    (window as unknown as { __transferReady: boolean }).__transferReady = true;
  });
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeEnabled();
  await expect(bubble.getByRole("progressbar")).toHaveCount(0);
  await tray.getByRole("button", { name: "Save", exact: true }).click();
  await expect(tray.getByRole("button", { name: "Reveal" })).toBeVisible();
  expect(
    await page.evaluate(
      () => (window as unknown as { __transferSaves: number }).__transferSaves,
    ),
  ).toBe(1);
  await page.keyboard.press("Escape");
  await page.getByTestId("conversation-history-trigger").click();
  await history.getByRole("tab", { name: /Files/ }).click();
  await expect(
    historyFile.getByRole("button", { name: "Reveal" }),
  ).toBeEnabled();
});
