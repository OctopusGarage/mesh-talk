import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat } from "./helpers/session";

for (const viewport of [
  { width: 1280, height: 800, cost: "restricted" as const },
  { width: 760, height: 620, cost: "unknown" as const },
]) {
  test(`attachment protection preserves sharing intent at ${viewport.width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await enterChat(page);
    await page.evaluate(
      async ({ channelId, cost }) => {
        type Host = {
          __TAURI_INTERNALS__: {
            invoke(
              command: string,
              args?: Record<string, unknown>,
            ): Promise<unknown>;
          };
          __mockEmit(event: string, payload: unknown): void;
          __clearFileSharingRestriction(): void;
        };
        const host = window as unknown as Host;
        const original = host.__TAURI_INTERNALS__.invoke.bind(
          host.__TAURI_INTERNALS__,
        );
        const identity = (await original("owner_node_identity", {
          owner: "u_self",
        })) as Record<string, string>;
        let networkCost: string = cost;
        let allowed = false;
        let sharing = true;
        const emit = () =>
          host.__mockEmit("channel-changed", {
            ...identity,
            channel_id: channelId,
          });
        host.__TAURI_INTERNALS__.invoke = async (command, args) => {
          if (command === "channel_members" && args?.channelId === channelId) {
            const info = (await original(command, args)) as {
              members: Array<Record<string, unknown>>;
            };
            return {
              ...info,
              members: info.members.map((member) =>
                member.user_id === identity.device_id
                  ? {
                      ...member,
                      replica_enabled: sharing,
                      file_sharing: {
                        network_cost: networkCost,
                        paused: networkCost !== "unrestricted" && !allowed,
                        override_enabled: allowed,
                      },
                    }
                  : member,
              ),
            };
          }
          if (command === "set_group_file_sharing_override") {
            if (args?.owner !== "u_self" || args.networkCost !== networkCost)
              throw new Error("stale owner or network cost");
            allowed = args.enabled === true;
            emit();
            return null;
          }
          if (
            command === "set_channel_replica_enabled" &&
            args?.channelId === channelId
          ) {
            sharing = args.enabled === true;
            emit();
            return null;
          }
          return original(command, args);
        };
        host.__clearFileSharingRestriction = () => {
          networkCost = "unrestricted";
          allowed = false;
          emit();
        };
      },
      { channelId: CHANNEL.id, cost: viewport.cost },
    );

    await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    await page.getByTestId("members-trigger").click();
    await page.getByTestId("group-settings-trigger").click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText(
      viewport.cost === "unknown"
        ? "network cost is unknown"
        : "paused to save data",
    );
    await expect(dialog).toContainText("There is no daily upload cap");
    await expect(
      dialog.getByRole("button", { name: "Stop sharing", exact: true }),
    ).toBeVisible();
    await page.evaluate(async () => {
      await document.fonts.ready;
    });
    await page.screenshot({
      path: testInfo.outputPath("automatic-sharing-paused.png"),
    });

    await dialog
      .getByRole("button", { name: "Allow temporarily", exact: true })
      .click();
    await expect(dialog).toContainText(
      "Automatic attachment sharing is temporarily allowed",
    );
    await expect(
      dialog.getByRole("button", {
        name: "Resume data protection",
        exact: true,
      }),
    ).toBeVisible();
    await dialog
      .getByRole("button", { name: "Stop sharing", exact: true })
      .click();
    await expect(
      dialog.getByRole("button", { name: "Resume sharing", exact: true }),
    ).toBeVisible();
    await expect(
      dialog.getByRole("button", {
        name: "Resume data protection",
        exact: true,
      }),
    ).toHaveCount(0);

    await page.evaluate(() =>
      (
        window as unknown as { __clearFileSharingRestriction(): void }
      ).__clearFileSharingRestriction(),
    );
    await expect(
      dialog.getByRole("button", { name: "Resume sharing", exact: true }),
    ).toBeVisible();
  });
}
