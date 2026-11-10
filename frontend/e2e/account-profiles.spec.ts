import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });
async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
}
test("edits an ordinary profile with one nickname and read-only creation metadata", async ({
  page,
}, testInfo) => {
  await login(page);
  await page.getByTestId("open-profile").click();
  const panel = page.getByTestId("account-profile-panel");
  await expect(panel).toContainText("Unknown");
  await expect(panel.locator('input[type="date"]')).toHaveCount(0);
  await expect(panel.getByRole("textbox")).toHaveCount(1);
  await panel
    .getByLabel("About", { exact: true })
    .fill("A public introduction");
  await panel.getByRole("button", { name: "Save", exact: true }).click();
  await expect(panel.getByRole("status")).toHaveText("Profile saved");
  await page.getByTestId("profile-name").click();
  await page.getByTestId("profile-name-input").fill("Alice");
  await page.getByTestId("profile-name-save").click();
  await expect(page.getByTestId("profile-name-text")).toHaveText("Alice");
  await expect(panel).not.toContainText(/signed|witness|JSON|publish/i);
  await panel.getByText("Account ID and QR code", { exact: true }).click();
  await expect(
    panel.getByRole("img", { name: "Account ID QR code" }),
  ).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("profile-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });

  await page.screenshot({ path: testInfo.outputPath("profile-mobile.png") });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.keyboard.press("Escape");
  await page.getByTestId("open-profile").click();
  await expect(panel.getByLabel("About", { exact: true })).toHaveValue(
    "A public introduction",
  );
  await expect(page.getByTestId("profile-name-text")).toHaveText("Alice");
});
test("shows contact profile read-only while preserving private verification history", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("conversation-bio")).toHaveText(
    "Hello from Bob",
  );
  await page.getByTestId("conversation-member-profile").click();
  await expect(page.getByTestId("member-profile-dialog")).toContainText(
    "Hello from Bob",
  );
  await page.keyboard.press("Escape");
  await page.getByTestId("verify-trigger").click();
  const dialog = page.getByTestId("verify-dialog");
  const panel = dialog.getByTestId("account-profile-panel");
  await expect(panel).toContainText("Hello from Bob");
  await expect(panel.getByRole("textbox")).toHaveCount(0);
  await dialog
    .getByText("Private observation history", { exact: true })
    .click();
  await expect(dialog).toContainText("Stored only on this device.");
  await expect(dialog).not.toContainText(/signed|witness|JSON|export/i);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
    "data-trust",
    "unverified",
  );
});

test("updates header About from profile events and clears it when switching contacts", async ({
  page,
}, testInfo) => {
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("conversation-bio")).toHaveText(
    "Hello from Bob",
  );
  const bio = "A long personal status with 中文 and emoji 🌱. ".repeat(12);
  await page.evaluate((bio) => {
    const host = window as unknown as {
      __mockSetAccountBio: (id: string, bio: string) => void;
      __mockEmit: (name: string, payload: unknown) => void;
    };
    host.__mockSetAccountBio("acc_bob_bbbb2222", bio);
    host.__mockEmit("profile-received", {
      account_id: "acc_bob_bbbb2222",
      avatar: null,
    });
  }, bio);
  await expect(page.getByTestId("conversation-bio")).toHaveText(bio.trim());
  await expect(page.getByTestId("conversation-bio")).toHaveAttribute(
    "title",
    bio.trim(),
  );
  await expect(page.getByTestId("verify-trigger")).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("header-about-desktop.png"),
  });
  await page.setViewportSize({ width: 768, height: 844 });
  await expect(page.getByTestId("verify-trigger")).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("header-about-compact.png"),
  });
  const action = await page.getByTestId("verify-trigger").boundingBox();
  expect(action).not.toBeNull();
  expect(action!.x + action!.width).toBeLessThanOrEqual(768);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.setViewportSize({ width: 1100, height: 800 });
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await expect(page.getByTestId("conversation-bio")).toHaveCount(0);
});
