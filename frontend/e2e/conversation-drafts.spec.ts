import { test, expect } from "./tauri-mock";
import { BOB, CAROL, CHANNEL, enterChat } from "./helpers/session";

test("draft previews and exact composer text survive switching and reload", async ({
  page,
}) => {
  await enterChat(page);
  const bob = page.getByTestId(`conversation-row-${BOB.account}`);
  const carol = page.getByTestId(`conversation-row-${CAROL.account}`);
  const channel = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  const input = page.getByTestId("composer-input");
  const text = "Unfinished message\n中文 👋";
  await bob.click();
  await input.fill(text);
  await expect(bob).not.toContainText("[Draft]");
  await carol.click();
  await expect(bob).toContainText("[Draft] Unfinished message 中文 👋");
  await expect(input).toHaveValue("");
  await input.fill("Carol draft");
  await channel.click();
  await input.fill("Group draft");
  await bob.click();
  await expect(input).toHaveValue(text);
  await expect(carol).toContainText("[Draft] Carol draft");
  await expect(channel).toContainText("[Draft] Group draft");
  // Switching flushes pending edits; wait for the mocked native store before reloading.
  await expect
    .poll(() =>
      page.evaluate(() =>
        Object.keys(localStorage)
          .filter((key) => key.startsWith("mock-drafts-"))
          .map((key) => localStorage.getItem(key))
          .join(""),
      ),
    )
    .toContain("Group draft");
  await page.reload();
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await bob.click();
  await expect(input).toHaveValue(text);
  await input.fill("");
  await carol.click();
  await expect(bob).not.toContainText("[Draft]");
  await bob.click();
  await expect(input).toHaveValue("");
  await input.fill("Send this draft");
  await page.getByTestId("composer-send").click();
  await expect(input).toHaveValue("");
  await carol.click();
  await expect(bob).not.toContainText("[Draft]");
});

test("idle saves the active conversation without switching away", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await page.getByTestId("composer-input").fill("Save after I pause typing");
  await expect
    .poll(
      () =>
        page.evaluate(() =>
          localStorage.getItem("mock-drafts-u_self:acc_self_aaaa1111"),
        ),
      { timeout: 15000 },
    )
    .toContain("Save after I pause typing");
  await page.reload();
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("composer-input")).toHaveValue(
    "Save after I pause typing",
  );
});

test("draft previews remain contained in a narrow dark window", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 900, height: 650 });
  await page.addInitScript(() =>
    localStorage.setItem("mesh-talk-theme", "dark"),
  );
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await page.getByTestId("composer-input").fill("LongUnbrokenDraft".repeat(30));
  await page.getByTestId(`conversation-row-${CAROL.account}`).click();
  const row = page.getByTestId(`conversation-row-${BOB.account}`);
  await expect(row).toContainText("[Draft]");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("draft-preview-dark.png"),
  });
});
