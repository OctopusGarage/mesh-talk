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
