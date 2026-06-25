import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

const BOB = { account: "acc_bob_bbbb2222" };

async function enterBobDm(page: Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
}

test.use({ viewport: { width: 1100, height: 800 } });

test("recall a freshly-sent message → it becomes a placeholder", async ({
  page,
}) => {
  await enterBobDm(page);
  // Send a fresh message (recall-eligible because it's "now").
  await page.getByTestId("composer-input").fill("oops typo");
  await page.getByTestId("composer-send").click();
  const bubble = page.getByText("oops typo");
  await expect(bubble).toBeVisible();

  // Recall it via the context menu.
  await bubble.click({ button: "right" });
  await expect(page.getByTestId("message-context-menu")).toBeVisible();
  await page.getByTestId("msg-recall").click();

  // The content is gone, replaced by the "you recalled a message" placeholder.
  await expect(page.getByText("oops typo")).toHaveCount(0);
  await expect(page.getByTestId("message-recalled")).toBeVisible();
  await expect(page.getByText("You recalled a message")).toBeVisible();

  // "Re-edit" drops the original text back into the composer (WeChat behaviour).
  await page.getByTestId("msg-reedit").click();
  await expect(page.getByTestId("composer-input")).toHaveValue("oops typo");
});

test("delete a message removes it locally", async ({ page }) => {
  await enterBobDm(page);
  await page.getByTestId("composer-input").fill("delete me");
  await page.getByTestId("composer-send").click();
  const bubble = page.getByText("delete me");
  await expect(bubble).toBeVisible();

  await bubble.click({ button: "right" });
  await page.getByTestId("msg-delete").click();
  await expect(page.getByText("delete me")).toHaveCount(0);
});

test("a seeded (old) message offers delete but not recall", async ({ page }) => {
  await enterBobDm(page);
  // My seeded reply is older than the 2-minute window → no recall, but delete is allowed.
  await page.getByText("thanks! glad to be here").click({ button: "right" });
  await expect(page.getByTestId("message-context-menu")).toBeVisible();
  await expect(page.getByTestId("msg-delete")).toBeVisible();
  await expect(page.getByTestId("msg-recall")).toHaveCount(0);
});

test("clear chat history (from the history dialog) empties the conversation", async ({
  page,
}) => {
  await enterBobDm(page);
  // Clear lives inside the conversation-history dialog, not directly in the header.
  await page.getByTestId("conversation-history-trigger").click();
  await expect(page.getByTestId("conversation-history-dialog")).toBeVisible();
  await page.getByTestId("clear-history").click();
  await expect(page.getByTestId("clear-history-dialog")).toBeVisible();
  await page.getByTestId("clear-history-confirm").click();
  // The history dialog closes and the seeded messages are gone.
  await expect(page.getByText("hey, welcome to the mesh")).toHaveCount(0);
  await expect(page.getByTestId("message-bubble")).toHaveCount(0);
});

test("retention setting persists", async ({ page }) => {
  await enterBobDm(page);
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-settings").click();
  const sel = page.getByTestId("settings-retention-select");
  await expect(sel).toBeVisible();
  await sel.selectOption("30");
  await expect(sel).toHaveValue("30");
});
