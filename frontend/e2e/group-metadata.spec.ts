import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat } from "./helpers/session";

test("owner previews, publishes and deletes a durable group announcement", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  const dialog = page.getByRole("dialog");
  await dialog
    .getByRole("button", { name: "Write announcement", exact: true })
    .click();
  await dialog
    .getByRole("textbox", { name: "Group announcement", exact: true })
    .fill("Welcome to the group.\nPlease review the schedule.");
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(dialog.getByTestId("announcement-preview")).toHaveText(
    "Welcome to the group.\nPlease review the schedule.",
  );
  await dialog.getByRole("button", { name: "Publish", exact: true }).click();
  await expect(dialog.getByTestId("group-announcement-content")).toContainText(
    "Please review the schedule.",
  );
  await page.screenshot({
    path: test.info().outputPath("announcement-settings-desktop.png"),
  });
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("announcement-publication")).toHaveCount(1);
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  await expect(dialog.getByTestId("group-announcement-content")).toContainText(
    "Welcome to the group.",
  );
  await dialog
    .getByRole("button", { name: "Delete announcement", exact: true })
    .click();
  await expect(
    dialog.getByText("Remove the current announcement for group members?"),
  ).toBeVisible();
  await dialog
    .getByRole("button", { name: "Confirm deletion", exact: true })
    .click();
  await expect(dialog.getByTestId("group-announcement-content")).toHaveText(
    "No group announcement",
  );
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("announcement-publication")).toHaveCount(2);
});

test("group information fits a narrow window and cancels an unpublished draft", async ({
  page,
}) => {
  await page.setViewportSize({ width: 700, height: 650 });
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("button", { name: "Set group photo", exact: true }),
  ).toBeVisible();
  await dialog
    .getByRole("button", { name: "Write announcement", exact: true })
    .click();
  await dialog
    .getByRole("textbox", { name: "Group announcement", exact: true })
    .fill("未发布的群公告 " + "LongUnbrokenText".repeat(100));
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  await page.screenshot({
    path: test.info().outputPath("announcement-settings-narrow.png"),
  });
  expect(
    await dialog.evaluate(
      (element) => element.scrollWidth <= element.clientWidth,
    ),
  ).toBe(true);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog.getByTestId("group-announcement-content")).toHaveText(
    "No group announcement",
  );
});

test("shared photo follows private overrides and announcement reminders track exact versions", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  const shared =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l7sAAAAASUVORK5CYII=";
  await page.evaluate(
    async ({ id, photo }) => {
      const host = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke(
            command: string,
            args?: Record<string, unknown>,
          ): Promise<unknown>;
        };
        __mockEmit(event: string, payload: unknown): void;
        __nextGroupNotice(): void;
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
      let revision = 1;
      const metadata = () => ({
        cover: {
          revision: "cover",
          content: photo,
          author: "bob",
          authored_at: 1,
        },
        announcement: {
          revision: String(revision),
          content: `Notice ${revision}`,
          author: "bob",
          authored_at: Date.now(),
        },
        announcement_history: [],
      });
      host.__TAURI_INTERNALS__.invoke = async (command, args) =>
        command === "list_channels"
          ? channels.map((channel) =>
              channel.channel_id === id
                ? { ...channel, metadata: metadata() }
                : channel,
            )
          : command === "channel_metadata"
            ? metadata()
            : original(command, args);
      const push = () =>
        host.__mockEmit("channel-changed", { ...identity, channel_id: id });
      host.__nextGroupNotice = () => {
        revision++;
        push();
      };
      push();
    },
    { id: CHANNEL.id, photo: shared },
  );
  const headerPhoto = page
    .getByTestId("conversation-header")
    .locator('img[alt="team"]');
  await expect(headerPhoto).toHaveAttribute("src", shared);
  await expect(page.getByTestId("group-announcement-reminder")).toContainText(
    "Notice 1",
  );
  await page
    .getByRole("button", { name: "Dismiss announcement reminder", exact: true })
    .click();
  await expect(page.getByTestId("group-announcement-reminder")).toHaveCount(0);
  await page.evaluate(() =>
    (window as unknown as { __nextGroupNotice(): void }).__nextGroupNotice(),
  );
  await expect(page.getByTestId("group-announcement-reminder")).toContainText(
    "Notice 2",
  );
  await page
    .getByRole("button", { name: "My group photo", exact: true })
    .click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page
      .getByRole("button", { name: "Set my group photo…", exact: true })
      .click(),
  ]);
  await chooser.setFiles("../src-tauri/icons/icon.png");
  await page.getByRole("button", { name: "Set photo", exact: true }).click();
  await expect(headerPhoto).not.toHaveAttribute("src", shared);
  await page
    .getByRole("button", { name: "My group photo", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Use group photo", exact: true })
    .click();
  await expect(headerPhoto).toHaveAttribute("src", shared);
  await expect(
    page
      .getByTestId(`conversation-row-${CHANNEL.id}`)
      .locator('img[alt="team"]'),
  ).toHaveAttribute("src", shared);
});

test("owner publishes and removes the shared group photo through the crop flow", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.getByRole("button", { name: "Set group photo", exact: true }).click(),
  ]);
  await chooser.setFiles("../src-tauri/icons/icon.png");
  await page.getByRole("button", { name: "Set photo", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("img", { name: "Group photo", exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  const headerPhoto = page
    .getByTestId("conversation-header")
    .locator('img[alt="team"]');
  await expect(headerPhoto).toBeVisible();
  await expect(headerPhoto).toHaveAttribute("src", /^data:image\/jpeg;base64,/);
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  await page
    .getByRole("button", { name: "Remove group photo", exact: true })
    .click();
  await expect(
    dialog.getByRole("img", { name: "Group photo", exact: true }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(headerPhoto).toHaveCount(0);
});
