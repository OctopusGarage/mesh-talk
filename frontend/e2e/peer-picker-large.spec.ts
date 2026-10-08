import { test, expect } from "./tauri-mock";
import { enterChat, CHANNEL } from "./helpers/session";

test("large peer pickers keep a bounded DOM and reach distant rows by keyboard", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=peers-huge");
  await page.getByRole("button", { name: "New channel" }).click();
  const create = page.getByTestId("create-channel-dialog");
  const createList = create.locator("[data-virtual-index]");
  await expect.poll(() => createList.count()).toBeLessThan(40);
  await createList.first().locator("button").focus();
  await page.keyboard.press("End");
  const last = create.getByRole("button", { name: /Colleague 1000/ });
  await expect(last).toBeFocused();
  await last.click();
  await expect(last).toHaveAttribute("aria-pressed", "true");
  await create.getByRole("button", { name: "Cancel" }).click();

  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  const members = page.getByRole("dialog");
  const addable = members.locator("[data-virtual-index]");
  await expect.poll(() => addable.count()).toBeLessThan(40);
  await addable.first().locator("button").focus();
  await page.keyboard.press("End");
  await expect(
    members.getByRole("button", { name: /Colleague 1000/ }),
  ).toBeFocused();
});
