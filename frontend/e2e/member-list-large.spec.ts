import { test, expect } from "./tauri-mock";
import { enterChat, CHANNEL } from "./helpers/session";

test.use({ viewport: { width: 760, height: 520 } });

test("large channel member list stays bounded and keyboard reachable", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=members-huge");
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("1003");
  const rows = dialog.locator("[data-member-row]");
  await expect.poll(() => rows.count()).toBeLessThan(40);
  expect(
    await dialog.evaluate(
      (element) => element.scrollWidth - element.clientWidth,
    ),
  ).toBeLessThanOrEqual(1);
  await rows.first().focus();
  await page.keyboard.press("End");
  await expect(dialog.locator('[data-member-row="1002"]')).toBeFocused();
  await expect(
    dialog.locator('[data-member-row="1002"] span[title="Colleague 1000"]'),
  ).toBeVisible();
  await dialog.getByTestId("member-dm-device_member_bulk_999").click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByTestId("conversation-header")).toContainText(
    "Colleague 1000",
  );
});
