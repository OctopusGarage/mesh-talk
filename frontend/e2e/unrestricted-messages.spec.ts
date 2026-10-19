import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test("display-language settings remain available without message-language restrictions", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("settings-language-select")).toBeEnabled();
  await expect(page.getByTestId("settings-dm-language-select")).toHaveCount(0);
  await page
    .getByTestId("settings-dialog")
    .getByRole("button", { name: "Close", exact: true })
    .click();
  await openBobDm(page);
  await expect(page.getByTestId("dm-language-notice")).toHaveCount(0);
  for (const text of [
    "你好，今天我们一起讨论这个项目。",
    "明日の会議で話しましょう。",
    "نناقش المشروع اليوم",
    "We can discuss the project today.",
  ]) {
    await page.getByTestId("composer-input").fill(text);
    await page.getByTestId("composer-send").click();
    await expect(page.getByTestId("composer-input")).toHaveValue("");
    await expect(
      page.getByRole("log").getByText(text, { exact: true }),
    ).toBeVisible();
  }
});
