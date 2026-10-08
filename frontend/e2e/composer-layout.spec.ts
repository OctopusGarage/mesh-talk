import { test, expect } from "./tauri-mock";

// Less frequent composer actions are available from an explicit tools control.
const BOB = "acc_bob_bbbb2222";
test.use({ viewport: { width: 1100, height: 760 } });

test("composer reveals secondary actions on demand", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(page.getByTestId("composer-input")).toBeVisible();
  await expect(page.getByTestId("composer-more-tools")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await page.getByTestId("composer-more-tools").click();
  await expect(page.getByTestId("composer-more-tools")).toHaveAttribute(
    "aria-expanded",
    "true",
  );

  for (const id of [
    "composer-screenshot",
    "composer-attach",
    "composer-image", // dedicated send-picture button
    "composer-emoji",
    "composer-send",
  ]) {
    await expect(page.getByTestId(id)).toBeVisible();
  }

  // Every action button is above the input box (its bottom edge ≤ the input's top edge).
  const inp = (await page.getByTestId("composer-input").boundingBox())!;
  for (const id of [
    "composer-screenshot",
    "composer-attach",
    "composer-image",
    "composer-emoji",
  ]) {
    const b = (await page.getByTestId(id).boundingBox())!;
    expect(b.y + b.height).toBeLessThanOrEqual(inp.y + 2);
  }

  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-emoji-tab").focus();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("expression-picker")).toHaveCount(0);
  await expect(page.getByTestId("composer-emoji")).toBeFocused();

  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("expression-picker")).toBeVisible();
  await expect(page.getByTestId("expression-picker")).toHaveCSS(
    "transform",
    "matrix(1, 0, 0, 1, 0, 0)",
  );
});

test("Enter confirms IME composition before it can send", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await page.getByTestId(`conversation-row-${BOB}`).click();
  const input = page.getByTestId("composer-input");
  await input.fill("composing a message");
  await input.dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    bubbles: true,
    isComposing: true,
  });
  await expect(input).toHaveValue("composing a message");
  await expect(
    page.getByTestId("message-bubble").getByText("composing a message"),
  ).toHaveCount(0);

  await input.press("Enter");
  await expect(
    page.getByTestId("message-bubble").getByText("composing a message"),
  ).toBeVisible();
});
