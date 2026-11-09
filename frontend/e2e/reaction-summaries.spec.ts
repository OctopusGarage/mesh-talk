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

test("exact search context permits reaction inspection and paging without mutation", async ({
  page,
}) => {
  await enterChat(page, "reaction-context", "/?data=reactions-paged");
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("composer-input")).toBeEditable();
  await expect(page.getByTestId("reaction-chip")).toHaveCount(128);

  await page.getByTestId("sidebar-action-search").click();
  await page.getByTestId("search-input").fill("welcome");
  await expect(page.getByTestId("search-result")).toHaveCount(1);
  await page.getByTestId("search-result").click();
  await expect(page.getByTestId("search-dialog")).toBeHidden();
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "hey, welcome to the mesh" });
  // This is a read-only exact context within an otherwise writable chat.
  await expect(bubble.getByTestId("message-react")).toBeDisabled();
  await bubble
    .getByRole("button", { name: "More reactions", exact: true })
    .click();
  const dialog = page.getByTestId("reaction-summary-dialog");
  const additions = dialog.getByRole("button", { name: /Add reaction-/ });
  await expect(additions).toHaveCount(128);
  const next = dialog.getByRole("button", { name: "Next page", exact: true });
  await expect(next).toBeEnabled();
  await expect(additions.first()).toBeDisabled();
  await next.click();
  await expect(additions).toHaveCount(71);
  const start = dialog.getByRole("button", { name: "Start over", exact: true });
  await expect(start).toBeEnabled();
  await expect(additions.first()).toBeDisabled();
  await expect(
    dialog.getByRole("button", {
      name: "Remove your reaction-199 reaction",
      exact: true,
    }),
  ).toBeDisabled();
  await start.click();
  await expect(additions).toHaveCount(128);
  await expect(next).toBeEnabled();
  await expect(additions.first()).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  await page
    .getByRole("button", { name: "Jump to latest messages", exact: true })
    .first()
    .click();
  await expect(bubble.getByTestId("message-react")).toBeEnabled();
});
