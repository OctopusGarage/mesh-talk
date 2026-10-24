import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat } from "./helpers/session";

test("a pushed group change updates an open member list without a message or reopening", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await expect(page.getByRole("dialog")).toBeVisible();

  await page.evaluate(async (channelId) => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args?: Record<string, unknown>,
        ): Promise<unknown>;
      };
      __mockEmit(event: string, payload: unknown): void;
    };
    const original = host.__TAURI_INTERNALS__.invoke.bind(
      host.__TAURI_INTERNALS__,
    );
    const identity = (await original("owner_node_identity", {
      owner: "u_self",
    })) as {
      owner: string;
      account_id: string;
      device_id: string;
    };
    const channels = (await original("list_channels")) as Array<
      Record<string, unknown>
    >;
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "list_channels")
        return channels.map((channel) =>
          channel.channel_id === channelId
            ? {
                ...channel,
                name: "Updated team",
                member_count: 2,
                owner: "device_bob_2222",
              }
            : channel,
        );
      if (command === "channel_members" && args?.channelId === channelId)
        return {
          owner: "device_bob_2222",
          members: [
            {
              user_id: "device_self_0001",
              name: "tester",
              replica_enabled: true,
            },
            { user_id: "device_bob_2222", name: "bob", replica_enabled: false },
          ],
        };
      return original(command, args);
    };
    for (let i = 0; i < 20; i++)
      host.__mockEmit("channel-changed", {
        ...identity,
        channel_id: channelId,
      });
  }, CHANNEL.id);

  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByTestId("member-profile-device_carol_3333"),
  ).toHaveCount(0);
  await expect(
    dialog.getByTestId("member-replica-disabled-device_bob_2222"),
  ).toBeVisible();
  await expect(dialog.getByTestId("dissolve-channel-trigger")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("conversation-header")).toContainText(
    "Updated team",
  );
});

test("shared inventory keeps exact group IDs read-only until verified state arrives", async ({
  page,
}) => {
  await enterChat(page);
  await expect(page.getByTestId("conversation-row-Labali")).toHaveCount(0);
  await page.evaluate(async () => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args?: Record<string, unknown>,
        ): Promise<unknown>;
      };
      __mockEmit(event: string, payload: unknown): void;
      __inventoryActual?: boolean;
      __inventoryPush?: () => void;
    };
    const original = host.__TAURI_INTERNALS__.invoke.bind(
      host.__TAURI_INTERNALS__,
    );
    const identity = (await original("owner_node_identity", {
      owner: "u_self",
    })) as Record<string, unknown>;
    const channels = (await original("list_channels")) as Array<
      Record<string, unknown>
    >;
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "list_channels")
        return [
          ...channels,
          ...["Labali", "labali"].map((id) => ({
            channel_id: id,
            name: id,
            member_count: 256,
            owner: "device_self_0001",
            can_manage: true,
            shared_reference: !host.__inventoryActual,
            reference_members_complete: false,
            legacy_membership: id === "Labali",
            archived: !host.__inventoryActual,
            left: !host.__inventoryActual,
            dissolved: !host.__inventoryActual,
            pending_invite: !host.__inventoryActual,
          })),
        ];
      if (
        command === "channel_members" &&
        ["Labali", "labali"].includes(String(args?.channelId))
      )
        return {
          owner: "device_self_0001",
          can_manage: true,
          history_policy: "off",
          members: [
            {
              user_id: "device_bob_2222",
              account_id: "acc_bob_bbbb2222",
              name: "Cached old name",
            },
            {
              user_id: "device_bob_linked",
              account_id: "acc_bob_bbbb2222",
              name: "Other old name",
            },
            {
              user_id: "device_self_0001",
              account_id: identity.account_id,
              name: "tester",
              pending: true,
            },
          ],
        };
      return original(command, args);
    };
    host.__inventoryPush = () =>
      host.__mockEmit("channel-changed", { ...identity, channel_id: "Labali" });
    host.__inventoryPush();
  });
  for (const id of ["Labali", "labali"])
    await expect(page.getByTestId(`conversation-row-${id}`)).toBeVisible();
  await page.getByTestId("conversation-row-labali").click();
  await expect(
    page.getByText(/require verified group state on this device/),
  ).toBeVisible();
  await expect(page.getByTestId("composer-input")).toHaveCount(0);
  await page.getByTestId("conversation-row-Labali").click();
  await expect(page.getByTestId("conversation-header")).toContainText(
    "Shared group information · 256 members",
  );
  await expect(
    page.getByText(/group-format and account-membership update/),
  ).toBeVisible();
  await expect(page.getByTestId("composer-input")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: /Accept invitation/i }),
  ).toHaveCount(0);
  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("256 members");
  await expect(dialog).toContainText("Only known members are shown");
  await expect(dialog.locator("[data-member-row]")).toHaveCount(2);
  await expect(
    page
      .getByTestId("conversation-header")
      .getByTestId("group-avatar")
      .locator(":scope > *"),
  ).toHaveCount(1);
  await expect(dialog).not.toContainText("Cached old name");
  for (const name of [
    /Leave channel/i,
    /Disable replica/i,
    /Enable replica/i,
    /Transfer ownership/i,
    /Add linked device/i,
  ])
    await expect(dialog.getByRole("button", { name })).toHaveCount(0);
  await expect(dialog.getByTestId("dissolve-channel-trigger")).toHaveCount(0);
  await expect(dialog.locator('[data-testid^="member-invite-"]')).toHaveCount(
    0,
  );
  await page.setViewportSize({ width: 320, height: 720 });
  await expect
    .poll(async () => (await dialog.boundingBox())?.width)
    .toBeLessThanOrEqual(320);
  const bounds = await dialog.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(321);
  expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
    true,
  );
  await page.keyboard.press("Escape");
  await page.evaluate(() => {
    const host = window as unknown as {
      __inventoryActual: boolean;
      __inventoryPush(): void;
    };
    host.__inventoryActual = true;
    host.__inventoryPush();
  });
  await expect(page.getByTestId("composer-input")).toBeVisible();
  await expect(page.getByTestId("conversation-header")).not.toContainText(
    "Shared group information",
  );
  await expect(
    page.getByText(/group-format and account-membership update/),
  ).toHaveCount(0);
  await expect(page.getByTestId("conversation-row-Labali")).toHaveCount(1);
  await page.evaluate(async () => {
    const path = "/src/store/auth.ts";
    const { useAuth } = await import(/* @vite-ignore */ path);
    await useAuth.getState().logout();
  });
  await expect(page.getByTestId("conversation-row-Labali")).toHaveCount(0);
  await expect(page.getByTestId("composer-input")).toHaveCount(0);
});
