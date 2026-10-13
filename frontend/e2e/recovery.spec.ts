import { test, expect } from "./tauri-mock";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

const KEY =
  "11111111-22222222-33333333-44444444-55555555-66666666-77777777-88888888";
const preview = {
  account_id: "a".repeat(32),
  created_at_ms: 1_700_000_000_000,
  messages: 12,
  channels: 2,
  file_cards: 3,
  media_files: 2,
  missing_media: 1,
};

test.beforeEach(async ({ page }) => {
  await page.addInitScript(
    ({ key, backup }) => {
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
      ipc.invoke = async (command, args) => {
        if (command === "plugin:dialog|open") return "/tmp/account.mtbackup";
        if (command === "plugin:dialog|save")
          return "/tmp/new-account.mtbackup";
        if (command === "prepare_backup_key") return key;
        if (command === "backup_status") return null;
        if (command === "create_backup")
          return { recovery_key: key, preview: backup, archive_bytes: 2048 };
        if (command === "inspect_backup") {
          if (args.recoveryKey !== key)
            throw new Error("Incorrect recovery key or damaged backup");
          return backup;
        }
        if (command === "restore_backup")
          return {
            username: args.username,
            device_id: "b".repeat(32),
            preview: backup,
          };
        return original(command, args);
      };
    },
    { key: KEY, backup: preview },
  );
});

test("export asks for the key challenge before writing the backup", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page
    .getByRole("button", { name: "Create encrypted backup" })
    .last()
    .click();
  await expect(page.getByTestId("recovery-export-key")).toContainText(KEY);
  await expect(page.getByTestId("recovery-export")).toBeDisabled();
  await page.getByTestId("recovery-key-check").fill("22222222-77777777");
  await page.getByTestId("recovery-export").click();
  await expect(page.getByText("Backup saved and verified")).toBeVisible();
  await expect(page.getByText(/Last backup:/)).toContainText(
    "new-account.mtbackup",
  );
});

test("restore verifies the archive, shows its limits, and offers a fresh identity if needed", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 760 });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Restore from encrypted backup" })
    .click();
  await page.getByRole("button", { name: "Choose backup file" }).click();
  await page.getByTestId("restore-key").fill("wrong key");
  await page.getByTestId("restore-inspect").click();
  await expect(page.getByRole("alert")).toContainText(
    "Incorrect recovery key or damaged backup",
  );
  await page.getByTestId("restore-key").fill(KEY);
  await page.getByTestId("restore-inspect").click();
  await expect(page.getByTestId("restore-preview")).toContainText(
    "12 messages",
  );
  await expect(page.getByTestId("restore-preview")).toContainText(
    "1 file cards have no local media bytes",
  );
  await expect(page.getByTestId("restore-preview")).toContainText(
    "Peer sync cannot restore channel messages that were not in the backup",
  );
  await page.getByRole("checkbox", { name: /old device may be lost/i }).check();
  await expect(page.getByTestId("restore-commit")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Start with a new identity" }),
  ).toBeVisible();
  await page
    .getByRole("checkbox", { name: /old device may be lost/i })
    .uncheck();
  await page.getByTestId("restore-username").fill("recovered");
  await page.getByTestId("restore-password").fill("new-password");
  await page.getByTestId("restore-commit").click();
  await expect(page.getByText("Recovery complete")).toBeVisible();
  await expect(page.getByText("New device fingerprint:")).toBeVisible();
});

test("recovered channel history is readable without send actions", async ({
  page,
}) => {
  await page.addInitScript(() => {
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
    ipc.invoke = async (command, args) => {
      const result = await original(command, args);
      if (command === "list_channels" && Array.isArray(result))
        return result.map((channel: Record<string, unknown>) => ({
          ...channel,
          archived: true,
        }));
      return result;
    };
  });
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await expect(
    page.getByTestId("message-bubble").getByText("channel kickoff"),
  ).toBeVisible();
  await expect(
    page.getByText(/Ask the channel owner to invite this new device/),
  ).toBeVisible();
  await expect(page.getByPlaceholder("Message #team")).toHaveCount(0);
});
