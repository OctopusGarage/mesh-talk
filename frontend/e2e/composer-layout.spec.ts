import { test, expect } from "./tauri-mock";

// The composer action buttons (screenshot · attach · image · emoji) live in a toolbar
// ABOVE the input, not crammed onto the typing line; send stays on the input row.
const BOB = "acc_bob_bbbb2222";
test.use({ viewport: { width: 1100, height: 760 } });

test("composer actions sit in a toolbar above the input", async ({ page }) => {
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
});
