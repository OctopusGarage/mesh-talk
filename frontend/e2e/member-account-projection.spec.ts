import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat } from "./helpers/session";

test.use({ viewport: { width: 1100, height: 800 } });

test("linked devices render as one member and native owner-account authority applies on a sibling", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(
    ({ account, device, channel }) => {
      const win = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = async (command, args) => {
        if (command === "channel_members")
          return {
            owner: "original-owner-device",
            owner_account_id: "acc_self_aaaa1111",
            can_manage: true,
            history_policy: "off",
            members: [
              {
                user_id: "device_self_linked",
                account_id: "acc_self_aaaa1111",
                name: "Messi10",
              },
              {
                user_id: "device_self_0001",
                account_id: "acc_self_aaaa1111",
                name: "Old owner name",
              },
              { user_id: device, account_id: account, name: "Ronaldo" },
            ],
          };
        const result = await original(command, args);
        if (command === "list_accounts")
          return [
            ...(result as object[]),
            {
              account_id: "acc_self_aaaa1111",
              names: ["Canonical owner"],
              device_count: 2,
            },
          ];
        if (command === "get_presence")
          return {
            ...(result as object),
            [channel]: { online: true, member_count: 2, online_count: 2 },
          };
        return result;
      };
    },
    { ...BOB, channel: CHANNEL.id },
  );
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.locator("[data-member-row]")).toHaveCount(2);
  await expect(dialog.getByRole("heading", { level: 2 })).toContainText(
    "2 members",
  );
  await expect(dialog.getByTestId("member-owner-badge")).toHaveCount(1);
  await expect(
    dialog.getByTestId("member-profile-device_self_0001"),
  ).toContainText("Canonical owner");
  await expect(dialog.getByText("Messi10")).toHaveCount(0);
  await expect(dialog.getByTestId("member-dm-device_self_linked")).toHaveCount(
    0,
  );
  await expect(dialog.getByTestId(`member-dm-${BOB.device}`)).toHaveCount(1);
  await dialog
    .getByTestId("member-devices-account:acc_self_aaaa1111")
    .locator("summary")
    .click();
  const linked = dialog.getByTestId("member-device-device_self_linked");
  await expect(linked).toBeVisible();
  await expect(
    linked.getByRole("button", { name: /Transfer ownership/ }),
  ).toHaveCount(0);
  await expect(dialog.getByTestId("dissolve-channel-trigger")).toBeVisible();
  await expect(linked.getByRole("button", { name: /Remove/ })).toHaveCount(0);
  await expect(
    dialog.getByTestId("member-remove-account-acc_self_aaaa1111"),
  ).toHaveCount(0);
});

for (const allowed of [true, false]) {
  test(`sidebar rename uses native account authority ${allowed}`, async ({
    page,
  }) => {
    await page.addInitScript((canManage) => {
      const host = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      const original = host.__TAURI_INTERNALS__.invoke;
      host.__TAURI_INTERNALS__.invoke = async (command, args) => {
        const result = await original(command, args);
        if (command !== "list_channels") return result;
        return (result as Array<Record<string, unknown>>).map((channel) => ({
          ...channel,
          owner: canManage ? "original-owner-device" : "device_self_0001",
          owner_account_id: "acc_self_aaaa1111",
          can_manage: canManage,
        }));
      };
    }, allowed);
    await enterChat(page);
    await page.getByTestId(`conversation-row-${CHANNEL.id}`).hover();
    await page.getByTestId(`conversation-actions-${CHANNEL.id}`).click();
    await page.getByTestId(`conversation-rename-${CHANNEL.id}`).click();
    await expect(page.getByRole("dialog").getByRole("heading")).toHaveText(
      allowed ? "Rename channel" : "Rename contact",
    );
  });
}
