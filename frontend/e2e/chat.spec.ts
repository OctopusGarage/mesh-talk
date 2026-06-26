import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

// Stable scenario ids — mirror e2e/tauri-mock.ts.
const BOB = { account: "acc_bob_bbbb2222" };
const CAROL = { account: "acc_carol_cccc3333" };
const CHANNEL = { id: "chan_team_dddd4444" };

async function register(page: Page, user = "tester") {
  await page.goto("/");
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill(user);
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
}

async function signIn(page: Page, user = "tester") {
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill(user);
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
}

/** Full path to a rendered, ready chat shell with the roster loaded. */
async function enterChat(page: Page, user = "tester") {
  await register(page, user);
  await signIn(page, user);
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  // Roster has loaded once the seeded contact row is present (node boot resolved).
  await expect(
    page.getByTestId(`conversation-row-${BOB.account}`),
  ).toBeVisible();
}

/** Open Bob's DM and wait for its (seeded) history to render. */
async function openBobDm(page: Page) {
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
}

test.describe("Mesh-Talk UI flow", () => {
  test("register → sign in → chat shell renders", async ({ page }) => {
    await register(page);
    // Registration succeeds and flips back to sign-in with a notice.
    await expect(page.getByText("Account created")).toBeVisible();
    await signIn(page);

    await expect(page.getByTestId("chat-shell")).toBeVisible();
    // Identity is present (the glyph + own name), and no blank/black screen regression.
    await expect(page.getByTestId("self-identity")).toBeVisible();
    await expect(page.locator("#root")).not.toBeEmpty();
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
    await expect(page.getByText("Direct messages")).toBeVisible();
  });

  test("post-login view never collapses to a blank/black screen", async ({
    page,
  }) => {
    await enterChat(page, "guard");
    await expect(page.locator("#root")).not.toBeEmpty();
    await expect(page.getByTestId("chat-shell")).toBeVisible();
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("conversation list shows peers with presence; switch DM ↔ channel", async ({
    page,
  }) => {
    await enterChat(page);
    // Both seeded contacts and the channel are listed.
    await expect(
      page.getByTestId(`conversation-row-${BOB.account}`),
    ).toBeVisible();
    await expect(
      page.getByTestId(`conversation-row-${CAROL.account}`),
    ).toBeVisible();
    await expect(
      page.getByTestId(`conversation-row-${CHANNEL.id}`),
    ).toBeVisible();

    // Open the DM, then switch to the channel — header + history follow.
    await openBobDm(page);
    await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    await expect(page.getByText("channel kickoff")).toBeVisible();
    // Back to a DM.
    await page.getByTestId(`conversation-row-${CAROL.account}`).click();
    await expect(page.getByTestId("conversation-empty")).toBeVisible();
  });

  test("compose + send a message shows an optimistic bubble", async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    const box = page.getByTestId("composer-input");
    await box.fill("hello e2e");
    await page.getByTestId("composer-send").click();
    await expect(page.getByText("hello e2e")).toBeVisible();
  });

  test("Enter sends, Shift+Enter inserts a newline", async ({ page }) => {
    await enterChat(page);
    await openBobDm(page);
    const box = page.getByTestId("composer-input");

    // Shift+Enter must NOT send — it adds a newline and the text stays in the box.
    await box.fill("line one");
    await box.press("Shift+Enter");
    await box.pressSequentially("line two");
    await expect(box).toHaveValue("line one\nline two");

    // Enter sends and clears the composer.
    await box.press("Enter");
    await expect(box).toHaveValue("");
    await expect(page.getByText("line one")).toBeVisible();
  });

  test("URLs in messages render as clickable links", async ({ page }) => {
    await enterChat(page);
    await openBobDm(page);
    const box = page.getByTestId("composer-input");
    await box.fill("visit https://example.com today");
    await page.getByTestId("composer-send").click();
    await expect(
      page.getByRole("link", { name: "https://example.com" }),
    ).toBeVisible();
  });

  test("emoji picker inserts into the composer", async ({ page }) => {
    await enterChat(page);
    await openBobDm(page);
    await page.getByTestId("composer-emoji").click();
    await expect(page.getByTestId("emoji-picker")).toBeVisible();
    await page.getByTestId("emoji-option-🎉").click();
    await expect(page.getByTestId("composer-input")).toHaveValue("🎉");
  });

  test("screenshot button opens a two-option menu and captures", async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    // The screenshot button lives in the composer toolbar.
    await page.getByTestId("composer-screenshot").click();
    // The menu offers both capture modes.
    await expect(page.getByTestId("screenshot-menu")).toBeVisible();
    await expect(page.getByTestId("screenshot-now")).toBeVisible();
    await expect(page.getByTestId("screenshot-hidden")).toBeVisible();
    // Choosing "capture now" invokes the command (mock returns PNG bytes → image send),
    // the menu closes, and nothing errors out.
    await page.getByTestId("screenshot-now").click();
    await expect(page.getByTestId("screenshot-menu")).toHaveCount(0);
    await expect(page.getByText("Something went wrong")).toHaveCount(0);

    // The "hide window & capture" mode also runs cleanly.
    await page.getByTestId("composer-screenshot").click();
    await expect(page.getByTestId("screenshot-hidden")).toBeVisible();
    await page.getByTestId("screenshot-hidden").click();
    await expect(page.getByTestId("screenshot-menu")).toHaveCount(0);
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("add an emoji reaction and see the chip", async ({ page }) => {
    await enterChat(page);
    await openBobDm(page);
    // React to the first (seeded) incoming message.
    const firstBubble = page.getByTestId("message-bubble").first();
    await firstBubble.hover();
    await firstBubble.getByTestId("message-react").click();
    await expect(page.getByTestId("reaction-picker")).toBeVisible();
    await page.getByTestId("reaction-option-🔥").click();
    await expect(page.getByTestId("reaction-chip").first()).toBeVisible();
    await expect(page.getByTestId("reaction-chip").first()).toContainText("🔥");
  });

  test("right-click a message bubble opens the context menu; Copy works", async ({
    page,
    context,
  }) => {
    // The custom menu calls navigator.clipboard.writeText — grant it in headless Chromium.
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await enterChat(page);
    await openBobDm(page);

    const firstBubble = page.getByTestId("message-bubble").first();
    // Right-click the bubble → our custom in-app menu opens (not the browser menu).
    await firstBubble
      .getByText("hey, welcome to the mesh")
      .click({ button: "right" });
    await expect(page.getByTestId("message-context-menu")).toBeVisible();
    const copy = page.getByTestId("msg-copy");
    await expect(copy).toBeVisible();
    await expect(page.getByTestId("msg-reply")).toBeVisible();
    await expect(page.getByTestId("msg-react")).toBeVisible();

    // Copy puts the message text on the clipboard and closes the menu — no error.
    await copy.click();
    await expect(page.getByTestId("message-context-menu")).toHaveCount(0);
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toBe("hey, welcome to the mesh");
    await expect(page.getByText("Something went wrong")).toHaveCount(0);

    // Reply from the context menu opens the composer reply banner.
    await firstBubble
      .getByText("hey, welcome to the mesh")
      .click({ button: "right" });
    await page.getByTestId("msg-reply").click();
    await expect(page.getByTestId("composer-reply-banner")).toBeVisible();
  });

  test("@-mention autocomplete inserts a mention", async ({ page }) => {
    await enterChat(page);
    // The channel has members (bob, carol) to mention.
    await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    await expect(page.getByText("channel kickoff")).toBeVisible();
    const box = page.getByTestId("composer-input");
    await box.fill("@ca");
    await expect(page.getByTestId("mention-popover")).toBeVisible();
    await page.getByTestId("mention-option-carol").click();
    await expect(box).toHaveValue("@carol ");
  });

  test("reply-to shows the banner and the parent snippet", async ({ page }) => {
    await enterChat(page);
    await openBobDm(page);
    const firstBubble = page.getByTestId("message-bubble").first();
    await firstBubble.hover();
    await firstBubble.getByTestId("message-reply").click();
    // Composer banner appears referencing the parent.
    await expect(page.getByTestId("composer-reply-banner")).toBeVisible();
    await expect(page.getByTestId("composer-reply-banner")).toContainText(
      "hey, welcome to the mesh",
    );
    // Send the reply; the new bubble carries a parent snippet.
    await page.getByTestId("composer-input").fill("replying now");
    await page.getByTestId("composer-send").click();
    await expect(page.getByText("replying now")).toBeVisible();
    await expect(
      page.getByTestId("message-parent-snippet").first(),
    ).toBeVisible();
  });

  test("search opens, queries, shows results, and navigates", async ({
    page,
  }) => {
    await enterChat(page);
    await page.getByTestId("sidebar-action-search").click();
    await expect(page.getByTestId("search-dialog")).toBeVisible();
    await page.getByTestId("search-input").fill("welcome");
    const result = page.getByTestId("search-result").first();
    await expect(result).toBeVisible();
    await result.click();
    // Navigated into the conversation containing the hit.
    await expect(page.getByTestId("conversation-header")).toBeVisible();
    await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
  });

  test("Files tray opens", async ({ page }) => {
    await enterChat(page);
    // "Received files" is a top-level action (not in the overflow menu).
    await page.getByTestId("sidebar-action-files").click();
    await expect(page.getByTestId("files-tray")).toBeVisible();
    await expect(page.getByText("Nothing received yet.")).toBeVisible();
  });

  test("Verify-contact dialog shows the safety number and marks verified", async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    await page.getByTestId("verify-trigger").click();
    await expect(page.getByTestId("verify-dialog")).toBeVisible();
    await expect(page.getByTestId("safety-number")).toBeVisible();
    await page.getByTestId("verify-mark-button").click();
    await expect(page.getByTestId("verify-verified-banner")).toBeVisible();

    // Regression: the header shield must reflect "verified" WITHOUT re-opening the dialog.
    // Close it, switch away and back so the dialog re-mounts fresh, and check the trigger.
    await page.keyboard.press("Escape");
    await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    await expect(page.getByTestId("conversation-header")).toBeVisible();
    await openBobDm(page);
    await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
      "data-trust",
      "verified",
    );
  });

  test("Settings: pick themes (light, oled, brand palette)", async ({
    page,
  }) => {
    await enterChat(page);
    // App boots dark.
    await expect(page.locator("html")).toHaveClass(/dark/);

    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-action-settings").click();
    await expect(page.getByTestId("theme-picker")).toBeVisible();

    await page.getByTestId("theme-light").click();
    await expect(page.locator("html")).not.toHaveClass(/dark/);

    await page.getByTestId("theme-oled").click();
    await expect(page.locator("html")).toHaveClass(/oled/);
    await expect(page.locator("html")).toHaveClass(/dark/);

    // A dark brand palette sets data-palette over the dark base.
    await page.getByTestId("theme-barcelona").click();
    await expect(page.locator("html")).toHaveAttribute(
      "data-palette",
      "barcelona",
    );
    await expect(page.locator("html")).toHaveClass(/dark/);

    // Messi is the light brand palette — data-palette set, but on the LIGHT base.
    await page.getByTestId("theme-messi").click();
    await expect(page.locator("html")).toHaveAttribute("data-palette", "messi");
    await expect(page.locator("html")).not.toHaveClass(/dark/);
  });

  test("Settings: switch language EN ↔ 中文", async ({ page }) => {
    await enterChat(page);
    await expect(page.getByText("Direct messages")).toBeVisible();

    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-action-settings").click();
    const lang = page.getByTestId("settings-language-select");
    await lang.selectOption("zh-Hans");
    // Sidebar section label re-renders in Simplified Chinese.
    await expect(page.getByText("私信")).toBeVisible();

    await lang.selectOption("en");
    await expect(page.getByText("Direct messages")).toBeVisible();
  });

  test("Diagnostics: peers / env / logs / troubleshoot present", async ({
    page,
  }) => {
    await enterChat(page);
    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-action-diagnostics").click();
    await expect(page.getByTestId("diagnostics-dialog")).toBeVisible();
    // Overview (default tab) shows the environment facts.
    await expect(page.getByText("This device")).toBeVisible();
    await expect(page.getByText("Environment")).toBeVisible();

    // Peers tab lists the discovered roster.
    await page.getByTestId("diagnostics-tab-peers").click();
    await expect(page.getByText("Discovered peers")).toBeVisible();
    // A discovered peer's address (unique to the peers panel) is shown.
    await expect(page.getByText("192.168.1.20:47100")).toBeVisible();

    // Logs tab.
    await page.getByTestId("diagnostics-tab-logs").click();
    await expect(
      page.getByRole("button", { name: "Reveal logs folder" }),
    ).toBeVisible();

    // Troubleshoot tab.
    await page.getByTestId("diagnostics-tab-help").click();
    await expect(page.getByText("Troubleshoot").first()).toBeVisible();
  });

  test("Link-device dialog opens", async ({ page }) => {
    await enterChat(page);
    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-action-link").click();
    await expect(page.getByTestId("link-device-dialog")).toBeVisible();
    await expect(page.getByText("Add another of your devices")).toBeVisible();
  });

  test("pin a contact moves it to the Pinned section", async ({ page }) => {
    await enterChat(page);
    const row = page.getByTestId(`conversation-row-${CAROL.account}`);
    await row.hover();
    await page.getByTestId(`conversation-pin-${CAROL.account}`).click();
    // The "Pinned" section appears.
    await expect(page.getByText("Pinned")).toBeVisible();
  });

  test("simulated inbound message lands in the open conversation", async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    // Persist a new incoming message into the mock store, then emit the inbound event —
    // the active-conversation handler reloads history, which now returns it.
    await page.evaluate(() => {
      const w = window as unknown as Record<string, unknown>;
      (w.__mockInject as (c: string, t: string, who: string) => void)(
        "acc:acc_bob_bbbb2222",
        "ping from bob",
        "device_bob_2222",
      );
      (w.__mockEmit as (e: string, p: unknown) => void)("dm-received", {
        from: "device_bob_2222",
        from_name: "bob",
        text: "ping from bob",
        reply_to: null,
      });
    });
    await expect(page.getByText("ping from bob")).toBeVisible();
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("sign out returns to the login screen", async ({ page }) => {
    await enterChat(page);
    await page.getByTestId("sidebar-overflow").click();
    await page.getByTestId("sidebar-sign-out").click();
    await expect(page.getByTestId("login-form")).toBeVisible();
    await expect(page.getByTestId("login-submit")).toBeVisible();
  });
});
