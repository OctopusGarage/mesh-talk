import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat } from "./helpers/session";

test("an invitation requires acceptance before composing", async ({ page }) => {
  await enterChat(page, "tester", "/?data=invite");
  await expect(
    page.getByTestId(`conversation-row-${CHANNEL.id}`),
  ).toContainText("Invitation pending");
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect(
    page.getByText(
      "Accept this invitation to join the channel and read new messages.",
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Accept invitation" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Accept invitation" }).click();
  await expect(
    page.getByText(
      "Accept this invitation to join the channel and read new messages.",
    ),
  ).not.toBeVisible();
  await expect(page.getByTestId("conversation-header")).toContainText(
    "3 members",
  );
  await page.getByTestId("members-trigger").click();
  await expect(
    page.getByRole("dialog").getByText("Invitation pending"),
  ).not.toBeVisible();
});

test("a channel can be hidden and restored locally", async ({ page }) => {
  await enterChat(page);
  const row = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  await expect(row).toBeVisible();
  await row.hover();
  await page.getByTestId(`conversation-actions-${CHANNEL.id}`).click();
  await page.getByRole("button", { name: "Hide channel" }).click();
  await expect(row).not.toBeVisible();
  await page.getByText("Hidden channels (1)").click();
  await page.getByRole("button", { name: "Restore" }).click();
  await expect(row).toBeVisible();
});

test("the owner transfers ownership before leaving", async ({ page }) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByRole("button", { name: "Transfer ownership bob" }).click();
  await expect(page.getByText(/Make bob the channel owner/)).toBeVisible();
  await page
    .getByRole("button", { name: "Transfer ownership", exact: true })
    .click();
  await page.getByRole("button", { name: "Leave channel" }).click();
  await expect(
    page.getByText(/Your account will leave this group/),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Leave channel", exact: true })
    .last()
    .click();
  await expect(
    page.getByTestId(`conversation-row-${CHANNEL.id}`),
  ).toContainText("Left channel");
});

test("left channel history can be retained then deleted from the chat list", async ({
  page,
}, testInfo) => {
  await enterChat(page);
  const row = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  await row.click();
  await page.getByTestId("members-trigger").click();
  await page.getByRole("button", { name: "Transfer ownership bob" }).click();
  await page
    .getByRole("button", { name: "Transfer ownership", exact: true })
    .click();
  await page.getByRole("button", { name: "Leave channel" }).click();
  await expect(page.getByRole("checkbox")).not.toBeChecked();
  await page
    .getByRole("button", { name: "Leave channel", exact: true })
    .last()
    .click();
  await expect(row).toContainText("Left channel");
  await row.click();
  await page.getByRole("button", { name: "Delete chat", exact: true }).click();
  await expect(page.getByTestId("delete-chat-dialog")).toContainText(
    "all your linked devices",
  );
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(row).toBeVisible();
  await row.hover();
  await page.getByTestId(`conversation-actions-${CHANNEL.id}`).click();
  await page.getByTestId(`conversation-delete-${CHANNEL.id}`).click();
  await page.screenshot({
    path: testInfo.outputPath("delete-left-channel.png"),
    animations: "disabled",
  });
  await page.evaluate(() =>
    (window as unknown as { __mockFailNext(cmd: string): void }).__mockFailNext(
      "owner_delete_left_channel",
    ),
  );
  await page
    .getByTestId("delete-chat-dialog")
    .getByRole("button", { name: "Delete chat", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("Try again");
  await expect(row).toBeVisible();
  await page
    .getByTestId("delete-chat-dialog")
    .getByRole("button", { name: "Delete chat", exact: true })
    .click();
  await expect(row).toHaveCount(0);
  await expect(page.getByRole("log")).toHaveCount(0);
});

test("leave and delete history removes the chat only after leaving", async ({
  page,
}) => {
  await enterChat(page);
  const row = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  await row.click();
  await page.getByTestId("members-trigger").click();
  await page.getByRole("button", { name: "Transfer ownership bob" }).click();
  await page
    .getByRole("button", { name: "Transfer ownership", exact: true })
    .click();
  await page.getByRole("button", { name: "Leave channel" }).click();
  await page.getByRole("checkbox").check();
  await page
    .getByRole("button", { name: "Leave channel", exact: true })
    .last()
    .click();
  await expect(row).toHaveCount(0);
});
