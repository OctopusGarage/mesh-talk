import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat } from "./helpers/session";

test("owner history settings offer four windows, persist after reopening and become read-only on transfer", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  const dialog = page.getByRole("dialog");
  const policy = dialog.getByRole("combobox", {
    name: "History for new members",
  });
  await expect(policy).toHaveValue("last_ten_within_three_days");
  await expect(policy.locator("option")).toHaveText([
    "Off",
    "Latest 10 messages",
    "Messages from the last 3 days",
    "Latest 10 messages from the last 3 days",
  ]);
  for (const value of [
    "off",
    "last_ten",
    "last_three_days",
    "last_ten_within_three_days",
  ]) {
    await policy.selectOption(value);
    await expect(policy).toBeEnabled();
    await page.keyboard.press("Escape");
    await expect(dialog).not.toBeVisible();
    await page.getByTestId("members-trigger").click();
    await page.getByTestId("group-settings-trigger").click();
    await expect(policy).toHaveValue(value);
  }
  await dialog.getByTestId("group-members-back").click();
  await dialog.getByRole("button", { name: "Transfer ownership bob" }).click();
  await dialog
    .getByRole("button", { name: "Transfer ownership", exact: true })
    .click();
  await dialog.getByTestId("group-settings-trigger").click();
  await expect(policy).toHaveCount(0);
  await expect(
    dialog.getByText("Latest 10 messages from the last 3 days", {
      exact: true,
    }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("group-settings-trigger").click();
  await expect(policy).toHaveCount(0);
  await expect(
    dialog.getByText("Latest 10 messages from the last 3 days", {
      exact: true,
    }),
  ).toBeVisible();
});
