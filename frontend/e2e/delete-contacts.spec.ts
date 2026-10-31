import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

const carol = "acc_carol_cccc3333";
const bob = "acc_bob_bbbb2222";
async function login(page: Page) {
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
}
async function confirmCarol(page: Page) {
  await page
    .getByTestId(`conversation-row-${carol}`)
    .click({ button: "right" });
  await page.getByTestId(`delete-contact-menu-${carol}`).click();
  await expect(page.getByTestId("delete-contact-cancel")).toBeFocused();
}
async function setCarolOnline(page: Page) {
  await page.evaluate((account) => {
    (
      window as unknown as {
        __mockSetPresence: (
          next: Record<string, { online: boolean; last_seen_secs: number }>,
        ) => void;
      }
    ).__mockSetPresence({ [account]: { online: true, last_seen_secs: 0 } });
  }, carol);
}

test("offline deletion requires confirmation and erases history across restart and rediscovery", async ({
  page,
}, testInfo) => {
  await login(page);
  await page.evaluate((account) => {
    (
      window as unknown as {
        __mockInject: (conv: string, text: string, who: string) => void;
      }
    ).__mockInject(
      `acc:${account}`,
      "private history to erase",
      "device_carol_3333",
    );
  }, carol);
  await page.getByTestId(`conversation-row-${carol}`).click();
  await expect(
    page.getByRole("log").getByText("private history to erase"),
  ).toBeVisible();
  await confirmCarol(page);
  await expect(page.getByTestId("delete-contact-dialog")).toContainText(
    "linked devices",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  const dialog = page.getByTestId("delete-contact-dialog");
  expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
    true,
  );
  await page.screenshot({
    path: testInfo.outputPath("delete-offline-contact-confirmation.png"),
    animations: "disabled",
  });
  await page.getByTestId("delete-contact-cancel").click();
  await expect(
    page.getByRole("log").getByText("private history to erase"),
  ).toBeVisible();
  await confirmCarol(page);
  await page.getByTestId("delete-contact-history").check();
  await page.getByTestId("delete-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${carol}`)).toHaveCount(0);
  await expect(page.getByRole("log")).toHaveCount(0);
  await login(page);
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${carol}`)).toHaveCount(0);
  await setCarolOnline(page);
  // Complete a real roster refresh before checking deletion survives discovery.
  // An absence checked before the periodic refresh would prove no refresh behavior.
  await page.evaluate(async () => {
    const path = "/src/store/chat.ts";
    const { useChat } = await import(/* @vite-ignore */ path);
    await useChat.getState().refreshRoster();
  });
  await expect(page.getByTestId(`conversation-row-${carol}`)).toHaveCount(0);
  await page.evaluate(
    (account) =>
      (
        window as unknown as {
          __mockInject(conv: string, text: string, who: string): void;
        }
      ).__mockInject(
        `acc:${account}`,
        "new incoming message",
        "device_carol_3333",
      ),
    carol,
  );
  await expect(page.getByTestId(`conversation-row-${carol}`)).toBeVisible({
    timeout: 8000,
  });
  await page.getByTestId(`conversation-row-${carol}`).click();
  await expect(
    page.getByRole("log").getByText("private history to erase"),
  ).toHaveCount(0);
});

test("online contacts offer deletion without blocking", async ({ page }) => {
  await login(page);
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await expect(page.getByTestId(`hide-contact-menu-${bob}`)).toBeVisible();
  await expect(page.getByTestId(`delete-contact-menu-${bob}`)).toBeVisible();
});

test("coming online during confirmation still allows personal deletion", async ({
  page,
}) => {
  await login(page);
  await confirmCarol(page);
  await setCarolOnline(page);
  await expect(page.getByTestId("delete-contact-confirm")).toBeEnabled();
  await expect(page.getByTestId("delete-contact-history")).not.toBeChecked();
  await page.getByTestId("delete-contact-cancel").click();
  await expect(page.getByTestId(`conversation-row-${carol}`)).toBeVisible();
});

test("failed deletion preserves the row and allows retry", async ({ page }) => {
  await login(page);
  await confirmCarol(page);
  await page.evaluate(() =>
    (
      window as unknown as { __mockFailNext: (cmd: string) => void }
    ).__mockFailNext("delete_contact"),
  );
  await page.getByTestId("delete-contact-confirm").click();
  await expect(page.getByRole("alert")).toContainText("Try again");
  await expect(page.getByTestId(`conversation-row-${carol}`)).toBeVisible();
  await page.getByTestId("delete-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${carol}`)).toHaveCount(0);
});
