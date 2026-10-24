import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, BOB } from "./helpers/session";
import type { Page } from "@playwright/test";
import type { AccountBindingChange } from "../src/lib/api/privacy";

test.use({ viewport: { width: 1100, height: 1000 } });
const first: AccountBindingChange = {
  device_id: "8c13d57c0123456789abcdef88a0504e",
  previous_account_id: BOB.account,
  current_account_id: "cc520123456789abcdef012345670414",
};
const second = { ...first, device_id: "123456789abcdef00123456789abcdef0" };

async function installChanges(page: Page) {
  await page.evaluate(
    (changes) => {
      const win = window as unknown as {
        __bindingChanges: typeof changes;
        __bindingAccepted: unknown[];
        __bindingVerified: number;
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      win.__bindingChanges = changes;
      win.__bindingAccepted = [];
      win.__bindingVerified = 0;
      const invoke = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = async (command, args) => {
        if (command === "get_account_binding_changes")
          return { owner: args.owner, changes: win.__bindingChanges };
        if (command === "accept_account_binding_change") {
          const change = args.change as (typeof changes)[number];
          if (
            !win.__bindingChanges.some(
              (current) => JSON.stringify(current) === JSON.stringify(change),
            )
          )
            throw {
              kind: "invalid-input",
              message: "Account change is no longer current",
            };
          win.__bindingAccepted.push({ owner: args.owner, change });
          win.__bindingChanges = win.__bindingChanges.filter(
            (current) => current.device_id !== change.device_id,
          );
          return {
            owner: args.owner,
            version: 1,
            invisible: false,
            allowed_accounts: [],
          };
        }
        if (command === "mark_verified") win.__bindingVerified++;
        return invoke(command, args);
      };
    },
    [first, second],
  );
}

test("ordinary contact profiles keep account recovery hidden without errors", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.getByTestId("verify-trigger").click();
  await expect(page.getByTestId("verify-dialog")).toBeVisible();
  await expect(page.getByTestId("account-profile-panel")).toBeVisible();
  await expect(page.getByTestId("account-binding-recovery")).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("recipient reviews exact account changes and accepts one device without verifying it", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await installChanges(page);
  await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
    "data-trust",
    "unverified",
  );
  await page.getByTestId("verify-trigger").click();
  const recovery = page.getByTestId("account-binding-recovery");
  await expect(recovery).toContainText(
    "A contact’s device is presenting a different account",
  );
  for (const id of [
    first.previous_account_id,
    first.current_account_id,
    first.device_id,
    second.device_id,
  ])
    await expect(recovery).toContainText(id);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __bindingAccepted: unknown[] })
          .__bindingAccepted,
    ),
  ).toEqual([]);

  for (const [size, width, height] of [
    ["desktop", 1100, 1000],
    ["narrow", 390, 844],
  ] as const) {
    await page.setViewportSize({ width, height });
    await recovery.scrollIntoViewIfNeeded();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    expect(
      await recovery.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
    ).toBe(true);
    await expect(recovery).toHaveScreenshot(
      `account-binding-recovery-${size}.png`,
      { animations: "disabled" },
    );
  }
  await recovery.getByTestId("accept-account-change").first().click();
  await expect(recovery.getByTestId("accept-account-change")).toHaveCount(1);
  await expect(recovery).toContainText(second.device_id);
  await expect(recovery.getByRole("status")).toContainText(
    "Queued messages can be delivered again",
  );
  const state = await page.evaluate(() => {
    const win = window as unknown as {
      __bindingAccepted: Array<{ owner: string; change: unknown }>;
      __bindingVerified: number;
    };
    return { accepted: win.__bindingAccepted, verified: win.__bindingVerified };
  });
  expect(state.accepted).toHaveLength(1);
  expect(state.accepted[0].change).toEqual(first);
  expect(state.verified).toBe(0);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
    "data-trust",
    "unverified",
  );
  await page.setViewportSize({ width: 1100, height: 1000 });
  const originalHistory = await page
    .getByTestId("message-bubble")
    .allTextContents();
  expect(originalHistory.length).toBeGreaterThan(0);
  // Reopen the old account: the accepted device no longer needs review, while
  // its second device still does. Opening the current account is another action.
  await page.getByTestId("verify-trigger").click();
  await expect(
    page
      .getByTestId("account-binding-recovery")
      .getByTestId("accept-account-change"),
  ).toHaveCount(1);
  await page
    .getByTestId("account-binding-recovery")
    .getByTestId("accept-account-change")
    .click();
  await page.getByTestId("open-current-account").click();
  await expect(page.getByTestId("verify-dialog")).toHaveCount(0);
  await expect(page.getByTestId("message-bubble")).toHaveCount(0);
  await openBobDm(page);
  expect(await page.getByTestId("message-bubble").allTextContents()).toEqual(
    originalHistory,
  );
});

test("recipient cannot accept a proposal that changed after opening the dialog", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await installChanges(page);
  await page.getByTestId("verify-trigger").click();
  const recovery = page.getByTestId("account-binding-recovery");
  await expect(recovery.getByTestId("accept-account-change")).toHaveCount(2);
  await page.evaluate(() => {
    (window as unknown as { __bindingChanges: unknown[] }).__bindingChanges =
      [];
  });
  await recovery.getByTestId("accept-account-change").first().click();
  await expect(recovery.getByRole("alert")).toContainText(
    "This account change is no longer current",
  );
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __bindingAccepted: unknown[] })
          .__bindingAccepted,
    ),
  ).toEqual([]);
});
