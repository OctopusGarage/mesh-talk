import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat } from "./helpers/session";

test.use({ viewport: { width: 1100, height: 800 } });

test("linked devices render as one member and native owner-account authority applies on a sibling", async ({
  page,
}, testInfo) => {
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
  await expect(dialog.getByRole("heading")).toContainText("2 members");
  await expect(dialog.getByTestId("member-owner-badge")).toHaveCount(1);
  await expect(
    dialog.getByTestId("member-profile-device_self_0001"),
  ).toContainText("Canonical owner");
  await expect(dialog.getByText("Messi10")).toHaveCount(0);
  await expect(dialog.getByTestId("member-dm-device_self_linked")).toHaveCount(
    0,
  );
  await expect(dialog.getByTestId(`member-dm-${BOB.device}`)).toHaveCount(1);
  await expect(dialog.locator('[data-testid^="member-devices-"]')).toHaveCount(
    0,
  );
  await expect(dialog.getByText(/Devices \(/)).toHaveCount(0);
  await expect(
    dialog.getByText(/device_self_linked|device_self_0001/),
  ).toHaveCount(0);
  await expect(
    dialog.getByRole("combobox", { name: "History for new members" }),
  ).toHaveCount(0);
  await expect(dialog.getByTestId("dissolve-channel-trigger")).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("members-desktop.png") });
  await page.setViewportSize({ width: 620, height: 700 });
  await page.screenshot({ path: testInfo.outputPath("members-narrow.png") });
  await dialog.getByTestId("group-settings-trigger").click();
  await expect(
    dialog.getByRole("heading", { name: "Group settings", exact: true }),
  ).toBeVisible();
  await expect(dialog.locator('[data-testid^="member-devices-"]')).toHaveCount(
    0,
  );
  await dialog.getByTestId("group-members-back").click();
  await expect(dialog.getByTestId("group-settings-trigger")).toBeFocused();
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

for (const width of [1300, 1000]) {
  test(`previous-account groups preserve history without current-account actions at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 800 });
    const previousName = "此前账号的历史群聊：保留本机记录与较长的群名称";
    const previousId = "chan_previous_account";
    await page.addInitScript(
      ({ id, name }) => {
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
          const channels = result as Array<Record<string, unknown>>;
          return [
            ...channels,
            {
              ...channels[0],
              channel_id: id,
              name,
              previous_account: "acc_previous",
              left: true,
              pending_invite: true,
              can_manage: false,
            },
          ];
        };
      },
      { id: previousId, name: previousName },
    );
    await enterChat(page);
    await expect(
      page.getByTestId(`conversation-row-${CHANNEL.id}`),
    ).toBeVisible();
    await expect(
      page.getByTestId(`conversation-row-${previousId}`),
    ).toHaveCount(0);
    const history = page.getByTestId("previous-account-groups");
    await expect(history).toBeVisible();
    await expect(history).not.toHaveAttribute("open", "");
    const summary = history.locator("summary");
    await summary.focus();
    await page.keyboard.press("Enter");
    const group = history.getByRole("button", { name: previousName });
    await expect(group).toBeVisible();
    await expect(group).toHaveAttribute("title", previousName);
    await group.click();
    await expect(page.getByTestId("conversation-header")).toContainText(
      "Previous account",
    );
    await expect(page.getByTestId("conversation-header")).not.toContainText(
      "Left channel",
    );
    await expect(page.getByTestId("conversation-empty")).toHaveText(
      "No messages are kept on this device.",
    );
    await expect(
      page.getByText(
        "This group belongs to a previous account. You can read the history kept on this device.",
      ),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Accept invitation" }),
    ).toHaveCount(0);
    await expect(
      page.getByTestId("pending-invite-decline-trigger"),
    ).toHaveCount(0);
    await expect(page.getByTestId("composer-more-tools")).toHaveCount(0);
    await page.screenshot({
      path: testInfo.outputPath(`previous-account-${width}.png`),
    });
  });
}
