import { expect, test } from "./tauri-mock";
import { enterChat, enterChatShell } from "./helpers/session";

const PERSONAL = "acc_self_aaaa1111";

for (const viewport of [
  { width: 1280, height: 800 },
  { width: 760, height: 620 },
]) {
  test(`personal notes work without peers at ${viewport.width}x${viewport.height}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await enterChatShell(page, "personal", "/?data=empty");
    const entry = page.getByTestId(`conversation-row-${PERSONAL}`);
    await expect(entry).toHaveCount(1);
    await expect(entry).toHaveAccessibleName(/Saved Messages/);
    await entry.click();
    const header = page.getByTestId("conversation-header");
    await expect(header).toContainText("Saved Messages");
    await expect(page.getByTestId("composer-expiry")).toHaveCount(0);
    await expect(header.getByTestId("conversation-member-profile")).toHaveCount(
      0,
    );
    await expect(
      header.getByRole("button", { name: /call|verify/i }),
    ).toHaveCount(0);
    await page
      .getByTestId("composer-input")
      .fill("A note for my other devices");
    await page.getByTestId("composer-send").click();
    const log = page.getByRole("log");
    await expect(
      log.getByText("A note for my other devices", { exact: true }),
    ).toHaveCount(1);
    await expect(
      log.getByText("Saved on this device", { exact: true }),
    ).toBeVisible();
    await expect(log.locator("[data-delivery]")).toHaveCount(0);
    await entry.click();
    await expect(
      log.getByText("A note for my other devices", { exact: true }),
    ).toHaveCount(1);
    await page.screenshot({ path: testInfo.outputPath("saved-messages.png") });
  });
}

test("personal entry participates in keyboard navigation without contact actions", async ({
  page,
}) => {
  await enterChat(page);
  const entry = page.getByTestId(`conversation-row-${PERSONAL}`);
  await entry.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("conversation-header")).toContainText(
    "Saved Messages",
  );
  await expect(
    page.getByTestId(`conversation-actions-${PERSONAL}`),
  ).toHaveCount(0);
  await expect(page.getByTestId(`contact-profile-${PERSONAL}`)).toHaveCount(0);
  await page.keyboard.press("ArrowDown");
  await expect(
    page.getByTestId("conversation-row-acc_bob_bbbb2222"),
  ).toBeFocused();
});

test("older linked devices explain the file metadata limitation without enabling downloads", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const api = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (command: string, args: unknown) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = api.invoke;
    api.invoke = (command, args) =>
      command === "get_device_sync_status"
        ? Promise.resolve([
            {
              device_id: "older-sibling",
              phase: "up_to_date",
              pending_attachments: 0,
              error: null,
              legacy_file_metadata: true,
            },
          ])
        : invoke(command, args);
  });
  await page.getByTestId("sidebar-overflow").click();
  await page
    .getByRole("button", { name: "Link a device", exact: true })
    .click();
  const dialog = page.getByTestId("link-device-dialog");
  await expect(
    dialog.getByText(
      "Upgrade this linked device to sync file information separately from file downloads.",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(
    dialog.getByRole("switch", { name: "Attachments", exact: true }),
  ).not.toBeChecked();
});
