import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("new channel peers show received account avatars without hiding device IDs", async ({
  page,
}) => {
  await enterChat(page);
  await expect(
    page.getByTestId("conversation-row-acc_bob_bbbb2222").locator("img"),
  ).toBeVisible();
  await page.getByRole("button", { name: "New channel" }).click();
  const bob = page
    .getByTestId("create-channel-dialog")
    .getByRole("button", { name: "bob" });
  await expect(bob.locator("img")).toHaveAttribute("src", /^data:image\//);
  await expect(bob).toContainText("devi … 2222");
});

test("a failed channel creation keeps the form and allows retry", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enterChat(page);
  await page.getByRole("button", { name: "New channel" }).click();
  const dialog = page.getByTestId("create-channel-dialog");
  const name = dialog.getByRole("textbox", { name: "Channel name" });
  await name.fill("Field notes");
  await dialog.getByRole("button", { name: "bob" }).click();

  const failure = "network unreachable — 東京 " + "long-device-name".repeat(12);
  await page.evaluate((message) => {
    (
      window as unknown as {
        __mockFailNext: (command: string, message: string) => void;
      }
    ).__mockFailNext("create_channel", message);
  }, failure);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeVisible();
  await expect(name).toHaveValue("Field notes");
  await expect(dialog.getByRole("button", { name: "bob" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("create-channel-error")).toContainText(failure);
  expect(
    await dialog.evaluate(
      (element) => element.scrollWidth - element.clientWidth,
    ),
  ).toBeLessThanOrEqual(1);

  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByTestId("conversation-header")).toContainText(
    "Field notes",
  );
});
