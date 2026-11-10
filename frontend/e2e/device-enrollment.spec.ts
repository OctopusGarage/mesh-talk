import { test, expect } from "./tauri-mock";

// A deterministic native enrollment boundary; no source password enters the page.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const host = window as unknown as {
      __enrollmentCalls: Array<{
        command: string;
        args: Record<string, unknown>;
      }>;
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    host.__enrollmentCalls = [];
    const original = host.__TAURI_INTERNALS__.invoke;
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      host.__enrollmentCalls.push({ command, args });
      if (command === "begin_device_enrollment") return { id: "test-attempt" };
      if (command === "probe_device_enrollment")
        return [
          {
            id: "verified-source",
            device_id: "source-device",
            account_id: "source-account",
            nickname: "Source nickname",
            addresses: ["192.0.2.4:48484"],
          },
        ];
      if (command === "cancel_device_enrollment") return null;
      if (command === "complete_device_enrollment")
        return {
          success: true,
          token: "fixture-session",
          user: { id: "u_self", username: "tester", display_name: "tester" },
        };
      return original(command, args);
    };
  });
});

test("links from a signed-out desktop without creating an unrelated account", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-link-device").click();
  const dialog = page.getByTestId("device-enrollment-dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("same login name and password");
  await expect(dialog.locator('input[type="password"]')).toHaveCount(0);
  await expect(page.getByTestId("enrollment-submit")).toBeDisabled();
  await page.locator("#enrollment-source").selectOption("verified-source");
  await expect(page.getByTestId("enrollment-source-account")).toHaveText(
    "source-account",
  );
  await page
    .locator("#enrollment-code")
    .fill("abcd abcd abcd abcd abcd abcd abcd abcd");
  await page.locator("#enrollment-consent").check();
  await page.getByTestId("enrollment-submit").click();
  await expect(page.getByTestId("login-form")).toBeHidden();
  await expect(page.getByTestId("self-identity")).toBeVisible();
  const commands = await page.evaluate(() =>
    (
      window as unknown as { __enrollmentCalls: Array<{ command: string }> }
    ).__enrollmentCalls.map((call) => call.command),
  );
  expect(commands).toContain("complete_device_enrollment");
  expect(commands).not.toContain("register");
  expect(commands).not.toContain("login");
});

test("keeps source consent reachable in a narrow window and cancels without signing in", async ({
  page,
}) => {
  await page.setViewportSize({ width: 420, height: 600 });
  await page.goto("/");
  await page.getByTestId("login-link-device").click();
  const dialog = page.getByTestId("device-enrollment-dialog");
  await page.locator("#enrollment-source").selectOption("verified-source");
  await page.locator("#enrollment-code").fill("a".repeat(32));
  await page.locator("#enrollment-consent").check();
  await expect(page.getByTestId("enrollment-submit")).toBeEnabled();
  expect(
    await dialog.evaluate((node) => node.scrollWidth <= node.clientWidth),
  ).toBe(true);
  await page.getByTestId("enrollment-cancel").click();
  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as { __enrollmentCalls: Array<{ command: string }> }
        ).__enrollmentCalls.some(
          (call) => call.command === "cancel_device_enrollment",
        ),
      ),
    )
    .toBe(true);
});
