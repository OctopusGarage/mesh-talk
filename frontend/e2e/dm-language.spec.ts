import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat, openBobDm } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test("incoming language preference saves, reopens, and returns to unrestricted", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const choice = page.getByTestId("settings-dm-language-select");
  await expect(choice).toBeEnabled();
  await expect(choice).toHaveValue("");
  await expect(choice.locator("option")).toHaveCount(11);
  await choice.selectOption("ja");
  await expect(choice).toBeEnabled();
  await expect(choice).toHaveValue("ja");
  await page
    .getByTestId("settings-dialog")
    .getByRole("button", { name: "Close", exact: true })
    .click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(choice).toHaveValue("ja");
  await choice.selectOption("");
  await expect(choice).toBeEnabled();
  await expect(choice).toHaveValue("");
});

test("a rejected DM retains its draft while allowed messages and channels send", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=dm-language");
  await openBobDm(page);
  await expect(page.getByTestId("dm-language-notice")).toContainText(
    "80% English",
  );
  const input = page.getByTestId("composer-input");
  await input.fill("你好，今天我们一起讨论这个项目。");
  await page.getByTestId("composer-send").click();
  await expect(page.getByRole("alert")).toContainText("Your draft is saved");
  await expect(input).toHaveValue("你好，今天我们一起讨论这个项目。");
  await expect(
    page.getByRole("log").getByText("你好，今天我们一起讨论这个项目。"),
  ).toHaveCount(0);
  await input.fill("We can discuss this project together today.");
  await page.getByTestId("composer-send").click();
  await expect(input).toHaveValue("");
  await expect(
    page
      .getByRole("log")
      .getByText("We can discuss this project together today."),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect(page.getByTestId("dm-language-notice")).toHaveCount(0);
  await input.fill("频道消息不受语言限制。");
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByRole("log").getByText("频道消息不受语言限制。"),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(input).toHaveValue("");
});
