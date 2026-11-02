import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("replaces reaction pages and removes an owned reaction outside the inline prefix", async ({
  page,
}) => {
  await enterChat(page, "reaction-pages", "/?data=reactions-paged");
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("reaction-chip")).toHaveCount(128);
  await page
    .getByRole("button", { name: "More reactions", exact: true })
    .first()
    .click();
  const dialog = page.getByTestId("reaction-summary-dialog");
  await expect(
    dialog.getByRole("button", { name: /Add reaction-/ }),
  ).toHaveCount(128);
  await dialog.getByRole("button", { name: "Next page", exact: true }).click();
  await expect(
    dialog.getByRole("button", { name: /Add reaction-/ }),
  ).toHaveCount(71);
  await expect(
    dialog.getByRole("button", {
      name: "Remove your reaction-199 reaction",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    dialog.getByRole("button", {
      name: "Add reaction-000 reaction",
      exact: true,
    }),
  ).toHaveCount(0);
  await dialog
    .getByRole("button", {
      name: "Remove your reaction-199 reaction",
      exact: true,
    })
    .click();
  await expect(
    dialog.getByRole("button", { name: "Next page", exact: true }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Next page", exact: true }).click();
  await expect(
    dialog.getByRole("button", {
      name: "Add reaction-199 reaction",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    dialog.getByRole("button", {
      name: "Remove your reaction-199 reaction",
      exact: true,
    }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});
