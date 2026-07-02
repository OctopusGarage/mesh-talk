import type { Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export const BOB = { account: "acc_bob_bbbb2222", device: "device_bob_2222" };
export const CAROL = {
  account: "acc_carol_cccc3333",
  device: "device_carol_3333",
};
export const CHANNEL = { id: "chan_team_dddd4444" };

export async function enterChat(page: Page, user = "tester") {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill(user);
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${BOB.account}`)).toBeVisible();
}

export async function openBobDm(page: Page) {
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
}
