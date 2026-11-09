import { revealComposerAttachments, waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";

// Less frequent composer actions are available from an explicit tools control.
const BOB = "acc_bob_bbbb2222";
test.use({ viewport: { width: 1100, height: 760 } });

test("composer reveals secondary actions on demand", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
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
    "composer-attachments",
    "composer-emoji",
    "composer-send",
  ]) {
    await expect(page.getByTestId(id)).toBeVisible();
  }

  // Secondary-toolbar controls remain above the input, including the attachment trigger.
  const inp = (await page.getByTestId("composer-input").boundingBox())!;
  for (const id of [
    "composer-screenshot",
    "composer-attachments",
    "composer-emoji",
  ]) {
    const b = (await page.getByTestId(id).boundingBox())!;
    expect(b.y + b.height).toBeLessThanOrEqual(inp.y + 2);
  }

  const attachmentActions = [
    "composer-attach",
    "composer-attach-directory",
    "composer-image",
    "composer-camera",
  ];
  for (const id of attachmentActions)
    await expect(page.getByTestId(id)).not.toBeVisible();
  await revealComposerAttachments(page);
  for (const id of attachmentActions)
    await expect(page.getByTestId(id)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("composer-attachments")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(page.getByTestId("composer-attachments")).toBeFocused();

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
    // Browsers may serialize an identity transform as either value.
    /^(none|matrix\(1, 0, 0, 1, 0, 0\))$/,
  );
  await expect(page.getByTestId("expression-picker")).toHaveCSS("opacity", "1");
});

test("Enter confirms IME composition before it can send", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
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

  // macOS WebKit can end composition before dispatching the confirming Enter.
  await input.dispatchEvent("compositionend", { data: "composing a message" });
  await input.dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    bubbles: true,
    isComposing: false,
    keyCode: 229,
  });
  await expect(input).toHaveValue("composing a message");
  await expect(
    page.getByTestId("message-bubble").getByText("composing a message"),
  ).toHaveCount(0);

  await input.press("Shift+Enter");
  await expect(input).toHaveValue("composing a message\n");
  await input.press("Enter");
  await expect(
    page.getByTestId("message-bubble").getByText("composing a message"),
  ).toBeVisible();
});
