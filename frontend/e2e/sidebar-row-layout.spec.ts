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
