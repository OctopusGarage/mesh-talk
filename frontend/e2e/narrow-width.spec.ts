import { test, expect } from "./tauri-mock";

// Regression: the conversation pane must adapt to the window width — a wide message must
// wrap inside the pane, never push it past the window edge (where it'd be clipped under the
// shell's overflow-hidden / hidden behind the sidebar). Guards the `min-w-0` on the pane
// and the message bubble's wrapping at a narrow window size.
const BOB = "acc_bob_bbbb2222";
const WIDE = "supercalifragilisticexpialidocious_".repeat(6) + " end";

test.use({ viewport: { width: 820, height: 700 } });

test("conversation pane adapts to a narrow window (no clipped messages)", async ({
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
  await page.getByTestId("composer-input").fill(WIDE);
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("message-bubble").last()).toBeVisible();

  const m = await page.evaluate(() => {
    const shell = document.querySelector(
      '[data-testid="chat-shell"]',
    ) as HTMLElement;
    const main = document.querySelector("main") as HTMLElement;
    const rows = document.querySelectorAll('[data-testid="message-bubble"]');
    const bubble = (rows[rows.length - 1] as HTMLElement).querySelector(
      ".rounded-2xl",
    ) as HTMLElement;
    const mr = main.getBoundingClientRect();
    const br = bubble.getBoundingClientRect();
    return {
      paneOverflows: main.scrollWidth > main.clientWidth + 1,
      paneWithinShell: Math.round(mr.right) <= shell.clientWidth + 1,
      bubbleWithinPane: br.right <= mr.right + 1 && br.left >= mr.left - 1,
    };
  });

  expect(m.paneOverflows).toBe(false);
  expect(m.paneWithinShell).toBe(true);
  expect(m.bubbleWithinPane).toBe(true);
});
