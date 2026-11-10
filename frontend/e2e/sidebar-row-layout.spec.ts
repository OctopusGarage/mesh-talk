import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat } from "./helpers/session";

test("short conversation names stay visible beside message times at minimum sidebar width", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem("mesh-talk-sidebar-width", "230");
  });
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();

  const unread = page.getByTestId("unread-review-trigger");
  const files = page.getByTestId("sidebar-action-files");
  const [unreadBox, filesBox] = await Promise.all([
    unread.boundingBox(),
    files.boundingBox(),
  ]);
  expect(Math.abs(unreadBox!.y - filesBox!.y)).toBeLessThanOrEqual(1);
  expect(unreadBox!.x + unreadBox!.width).toBeLessThanOrEqual(filesBox!.x);
  expect(unreadBox!.height).toBeGreaterThanOrEqual(44);
  expect(filesBox!.height).toBeGreaterThanOrEqual(44);
  await page
    .locator(".sidebar-tools")
    .screenshot({ path: test.info().outputPath("sidebar-tools-row.png") });
  await unread.click();
  await expect(
    page.getByRole("dialog", { name: "Unread messages", exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await files.click();
  await expect(page.getByTestId("files-tray")).toBeVisible();
  await page.keyboard.press("Escape");

  for (const [id, name] of [
    [BOB.account, "bob"],
    [CHANNEL.id, "team"],
  ]) {
    const row = page.getByTestId(`conversation-row-${id}`);
    const title = row.getByText(name, { exact: true });
    await expect(title).toBeVisible();
    const fits = await title.evaluate(
      (el) => el.scrollWidth <= el.clientWidth + 1,
    );
    expect(fits, `${name} should fit without being hidden by its time`).toBe(
      true,
    );
  }
});
