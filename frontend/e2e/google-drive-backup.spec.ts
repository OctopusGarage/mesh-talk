import { test, expect } from "./tauri-mock";
import { selectSettingsCategory } from "./helpers/settings";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
const key =
  "11111111-22222222-33333333-44444444-55555555-66666666-77777777-88888888";
const preview = {
  account_id: "a".repeat(32),
  created_at_ms: 1700000000000,
  messages: 12,
  channels: 2,
  file_cards: 3,
  media_files: 2,
  missing_media: 1,
};
test.beforeEach(async ({ page }) => {
  await page.addInitScript(
    ({ key, preview }) => {
      const ipc = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const original = ipc.invoke;
      let cancelDownload: (() => void) | null = null;

      (window as unknown as Record<string, unknown>).__driveCalls = [];
      ipc.invoke = async (command, args) => {
        if (command.startsWith("google_drive_"))
          (
            (window as unknown as Record<string, unknown>)
              .__driveCalls as string[]
          ).push(command);
        if (command === "google_drive_available") return true;
        if (command === "google_drive_disconnect") {
          cancelDownload?.();
          cancelDownload = null;
          return;
        }
        if (command === "google_drive_connect") return;
        if (command === "google_drive_download") {
          if (
            (window as unknown as Record<string, unknown>).__driveHoldDownload
          )
            return new Promise<void>((_resolve, reject) => {
              cancelDownload = () => reject(new Error("Transfer cancelled"));
            });
          return;
        }
        if (command === "google_drive_list")
          return [
            {
              id: "archive_1",
              name: "mesh-talk-test.mtbackup",
              size: "2048",
              checksum: "a".repeat(64),
            },
          ];
        if (command === "google_drive_progress")
          return { phase: "uploading", transferred: 1024, total: 2048 };
        if (command === "prepare_backup_key") return key;
        if (command === "backup_status") return null;
        if (command === "google_drive_create_backup") {
          if ((window as unknown as Record<string, unknown>).__driveFailUpload)
            throw new Error("Reconnect Google Drive and retry");
          return { recovery_key: key, preview, archive_bytes: 2048 };
        }
        if (command === "google_drive_inspect") {
          if (args.recoveryKey !== key)
            throw new Error("Incorrect recovery key or damaged backup");
          return preview;
        }
        if (command === "google_drive_restore")
          return {
            username: args.username,
            device_id: "b".repeat(32),
            preview,
          };
        return original(command, args);
      };
    },
    { key, preview },
  );
});
test("Drive export requires connection and a separately saved recovery key", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "history");
  await page
    .getByRole("button", { name: "Create encrypted backup" })
    .last()
    .click();
  await page.getByLabel("Backup destination").selectOption("drive");
  await expect(page.getByTestId("recovery-export")).toBeDisabled();
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.getByTestId("recovery-key-check").fill("22222222-77777777");
  await page.getByRole("button", { name: "Back up now", exact: true }).click();
  await expect(
    page.getByText("Encrypted backup saved to Google Drive"),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __driveCalls: string[] }).__driveCalls,
      ),
    )
    .toContain("google_drive_disconnect");
});
test("Drive recovery downloads, verifies, previews and explicitly restores", async ({
  page,
}) => {
  await page.goto("/");
  await page
    .getByRole("button", { name: "Restore from encrypted backup" })
    .click();
  await page.getByLabel("Backup source").selectOption("drive");
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.getByRole("button", { name: "Download selected backup" }).click();
  await page.getByTestId("restore-key").fill("wrong");
  await page.getByTestId("restore-inspect").click();
  await expect(page.getByRole("alert")).toContainText("Incorrect recovery key");
  await page.getByTestId("restore-key").fill(key);
  await page.getByTestId("restore-inspect").click();
  await expect(page.getByTestId("restore-preview")).toContainText(
    "12 messages",
  );
  await page.getByTestId("restore-username").fill("recovered");
  await page.getByTestId("restore-password").fill("newpassword123");
  await page.getByTestId("restore-commit").click();
  await expect(page.getByTestId("login-username")).toHaveValue("recovered");
});

test("failed cloud upload keeps the recovery key and offers reconnect and retry", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "history");
  await page
    .getByRole("button", { name: "Create encrypted backup" })
    .last()
    .click();
  await page.getByLabel("Backup destination").selectOption("drive");
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.getByTestId("recovery-key-check").fill("22222222-77777777");
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__driveFailUpload = true;
  });
  await page.getByRole("button", { name: "Back up now", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Reconnect Google Drive");
  await expect(
    page.getByText("Encrypted backup saved to Google Drive"),
  ).toHaveCount(0);
  await expect(page.getByTestId("recovery-export-key")).toContainText(key);
  await page
    .getByRole("button", { name: "Disconnect / change Google account" })
    .click();
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__driveFailUpload = false;
  });
  await page.getByRole("button", { name: "Back up now", exact: true }).click();
  await expect(
    page.getByText("Encrypted backup saved to Google Drive"),
  ).toBeVisible();
});

test("cancelled download cannot publish a preview and can reconnect", async ({
  page,
}) => {
  await page.goto("/");
  await page
    .getByRole("button", { name: "Restore from encrypted backup" })
    .click();
  await page.getByLabel("Backup source").selectOption("drive");
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__driveHoldDownload = true;
  });
  await page.getByRole("button", { name: "Download selected backup" }).click();
  await page.getByRole("button", { name: "Cancel transfer" }).click();
  await expect(page.getByTestId("restore-inspect")).toBeDisabled();
  await expect(page.getByTestId("restore-preview")).toHaveCount(0);
  await page.getByRole("button", { name: "Connect Google Drive" }).click();
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__driveHoldDownload = false;
  });
  await page.getByRole("button", { name: "Download selected backup" }).click();
  await expect(
    page.getByText("Backup downloaded. Enter its recovery key to verify it."),
  ).toBeVisible();
});
