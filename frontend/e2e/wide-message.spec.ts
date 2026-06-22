import { test, expect } from "./tauri-mock";

// Regression: a message with a very long unbreakable run (e.g. a ──── rule pasted from a
// terminal) plus space-padded "columns" must stay inside its bubble — the bubble must
// respect max-w and wrap, not blow out to full width and overflow off-screen (which hid
// the message details on the sender's side). Root cause was overflow-wrap:break-word
// (doesn't shrink min-content) + a missing min-w-0 on the flex chain.
const BOB = "acc_bob_bbbb2222";
const PAYLOAD = [
  "────────────────────────────────────────────────────────────────────────────────────────",
  "/exit                              Exit the CLI",
  "/labali-image-ocr-macos-vision     Run native macOS image OCR through",
].join("\n");

test.use({ viewport: { width: 1280, height: 800 } });

test("a very wide message stays inside its bubble (no overflow)", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await page.getByTestId("composer-input").fill(PAYLOAD);
  await page.getByTestId("composer-send").click();

  const row = page.getByTestId("message-bubble").last();
  await expect(row).toBeVisible();

  const m = await page.evaluate(() => {
    const log = document.querySelector('[role="log"]') as HTMLElement;
    const rows = document.querySelectorAll('[data-testid="message-bubble"]');
    const last = rows[rows.length - 1] as HTMLElement;
    const bubble = last.querySelector(".rounded-2xl") as HTMLElement;
    const lr = log.getBoundingClientRect();
    const br = bubble.getBoundingClientRect();
    return {
      logW: lr.width,
      bubbleW: br.width,
      overflowsRight: br.right > lr.right + 1,
      overflowsLeft: br.left < lr.left - 1,
    };
  });

  // The bubble must be bounded (well under full width) and within the conversation pane.
  expect(m.bubbleW).toBeLessThanOrEqual(m.logW * 0.75);
  expect(m.overflowsRight).toBe(false);
  expect(m.overflowsLeft).toBe(false);
});
