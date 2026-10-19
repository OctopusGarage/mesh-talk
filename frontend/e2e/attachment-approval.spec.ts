import { selectSettingsCategory } from "./helpers/settings";
import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, BOB } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test("incoming folder waits for approval and can be declined or received", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(({ account, device }) => {
    const win = window as unknown as Window & {
      __approved: boolean;
      __declined: boolean;
      __ready: boolean;
      __mockEmit: (name: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__approved = false;
    win.__declined = false;
    win.__ready = false;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "file_statuses")
        return (args.fileConvs as string[]).map((file_conv) => ({
          file_conv,
          done: win.__ready ? 2 : 0,
          total: 2,
          ready: win.__ready,
          consent: win.__declined
            ? "declined"
            : win.__approved
              ? "accepted"
              : "pending",
        }));
      if (cmd === "decide_attachment") {
        win.__approved = Boolean(args.accept);
        win.__declined = !win.__approved;
        return null;
      }
      const result = await original(cmd, args);
      if (cmd === "owner_account_history" && args.account === account)
        return [
          ...(result as unknown[]),
          {
            id: "folder-offer",
            from_me: false,
            who: device,
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              file_conv: "fc_offer_folder",
              name: "archive.tar",
              size: 4096,
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
      name: "archive.tar",
      size: 4096,
      mime: "application/x-mesh-talk-directory-tar",
      file_conv: "fc_offer_folder",
      media: false,
    });
  }, BOB);
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "archive" });
  await expect(
    bubble.getByText("Waiting for your approval to receive this attachment."),
  ).toBeVisible();
  await expect(bubble.getByRole("button", { name: "Save" })).toBeDisabled();
  await bubble.getByRole("button", { name: "Decline" }).click();
  await expect(bubble.getByText("You declined this attachment.")).toBeVisible();
  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await tray.getByRole("button", { name: "Receive" }).click();
  await expect(
    tray.getByText("Waiting for your approval to receive this attachment."),
  ).toHaveCount(0);
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await page.evaluate(() => {
    (window as unknown as { __ready: boolean }).__ready = true;
  });
  await expect(
    tray.getByRole("button", { name: "Save", exact: true }),
  ).toBeEnabled();
});

test("global and per-contact automatic receive controls persist their selection", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "files");
  const global = page.getByTestId("global-attachment-receive");
  await expect(global).toHaveValue("ask");
  await global.selectOption("auto");
  await expect(global).toHaveValue("auto");
  await page.keyboard.press("Escape");
  await openBobDm(page);
  await page.getByTestId("contact-settings-trigger").click();
  const contact = page.getByTestId("contact-attachment-receive");
  await expect(contact).toHaveValue("inherit");
  await contact.selectOption("ask");
  await expect(contact).toHaveValue("ask");
  await page.getByTestId("contact-settings-trigger").click();
  await page.getByTestId("contact-settings-trigger").click();
  await expect(contact).toHaveValue("ask");
});

test("video media requires approval and appears in Received files", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(({ account, device }) => {
    const win = window as unknown as Window & {
      __mockEmit: (name: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "file_statuses")
        return (args.fileConvs as string[]).map((file_conv) => ({
          file_conv,
          done: 0,
          total: 3,
          ready: false,
          consent: "pending",
        }));
      const result = await original(cmd, args);
      if (cmd === "owner_account_history" && args.account === account)
        return [
          ...(result as unknown[]),
          {
            id: "video-offer",
            from_me: false,
            who: device,
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              file_conv: "fc_offer_video",
              name: "clip.mp4",
              size: 10485760,
              mime: "video/mp4",
              media: true,
            },
          },
        ];
      return result;
    };
    win.__mockEmit("file-received", {
      conv: account,
      from: device,
      name: "clip.mp4",
      size: 10485760,
      mime: "video/mp4",
      file_conv: "fc_offer_video",
      media: true,
    });
  }, BOB);
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "clip.mp4" });
  await expect(bubble.getByRole("button", { name: "Receive" })).toBeVisible();
  await expect(bubble.getByRole("button", { name: "Save" })).toBeDisabled();
  await page.getByTestId("sidebar-action-files").click();
  await expect(
    page.getByTestId("files-tray").getByText("clip.mp4"),
  ).toBeVisible();
});
