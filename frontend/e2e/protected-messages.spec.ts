import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("protected text stays concealed in bubbles and sidebar, reveals temporarily and hides on blur", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-click").click();
  await page.keyboard.press("Escape");
  await page
    .getByTestId("composer-input")
    .fill("Private content for one reveal");
  await page.getByTestId("composer-send").click();
  const bubble = page.getByTestId("protected-message");
  await expect(bubble).toBeVisible();
  await expect(
    page.getByText("Private content for one reveal", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByTestId("conversation-row-acc_bob_bbbb2222"),
  ).not.toContainText("Private content for one reveal");
  await bubble.getByRole("button", { name: "Click to view" }).click();
  await expect(bubble.getByTestId("protected-message-body")).toHaveText(
    "Private content for one reveal",
  );
  await page.screenshot({ path: "test-results/protected-message-visible.png" });
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expect(bubble.getByTestId("protected-message-body")).toHaveCount(0);
  await expect(
    bubble.getByRole("button", { name: "Click to view" }),
  ).toBeVisible();
});

test("verification cancellation and failure remain concealed, every reveal expires and needs verification again", async ({
  page,
}) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
    }),
  );
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-authenticate").click();
  await page.getByTestId("composer-input").fill("Verified private text");
  await page.getByTestId("composer-send").click();
  const bubble = page.getByTestId("protected-message");
  await expect(bubble).toBeVisible();
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await bubble.getByRole("button", { name: "Cancel" }).click();
  await expect(bubble.getByTestId("protected-message-body")).toHaveCount(0);
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await bubble.getByLabel("Password", { exact: true }).fill("incorrect");
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await expect(bubble.getByRole("alert")).toBeVisible();
  await expect(bubble.getByTestId("protected-message-body")).toHaveCount(0);
  await page.clock.install();
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await bubble.getByLabel("Password", { exact: true }).fill("password123");
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await expect(bubble.getByTestId("protected-message-body")).toHaveText(
    "Verified private text",
  );
  await page.clock.fastForward(30000);
  await expect(bubble.getByTestId("protected-message-body")).toHaveCount(0);
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await expect(bubble.getByLabel("Password", { exact: true })).toBeEmpty();
  await bubble.getByLabel("Password", { exact: true }).fill("password123");
  await bubble.getByRole("button", { name: "Verify and view" }).click();
  await expect(bubble.getByTestId("protected-message-body")).toHaveText(
    "Verified private text",
  );
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await expect(page.getByTestId("protected-message-body")).toHaveCount(0);
});

test("protected composer and concealed bubble fit a narrow conversation", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-click").click();
  await page.getByTestId("composer-input").fill("Narrow private message");
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("protected-message")).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  const bounds = await page.getByTestId("composer").evaluate((dock) => {
    const pane = dock.getBoundingClientRect();
    const input = dock.querySelector("textarea")!.getBoundingClientRect();
    const send = dock
      .querySelector('[data-testid="composer-send"]')!
      .getBoundingClientRect();
    return {
      inputWidth: input.width,
      sendRight: send.right,
      paneRight: pane.right,
      inputLeft: input.left,
      paneLeft: pane.left,
    };
  });
  expect(bounds.inputWidth).toBeGreaterThanOrEqual(160);
  expect(bounds.inputLeft).toBeGreaterThanOrEqual(bounds.paneLeft);
  expect(bounds.sendRight).toBeLessThanOrEqual(bounds.paneRight);
  const bubbleBounds = await page
    .getByTestId("protected-message")
    .boundingBox();
  expect(bubbleBounds!.x).toBeGreaterThanOrEqual(bounds.paneLeft);
  expect(bubbleBounds!.x + bubbleBounds!.width).toBeLessThanOrEqual(
    bounds.paneRight,
  );
  await page.screenshot({
    path: "test-results/protected-message-minimum-window.png",
  });
});

test("keeps a protected draft's mode when switching conversations and returning", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-click").click();
  await page
    .getByTestId("composer-input")
    .fill("Protected draft across navigation");
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("composer-input")).toHaveValue(
    "Protected draft across navigation",
  );
  await expect(page.getByTestId("composer")).toContainText(
    "Text & images · hides after 30 seconds",
  );
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("protected-message")).toBeVisible();
  await expect(
    page.getByText("Protected draft across navigation", { exact: true }),
  ).toHaveCount(0);
});

test("clears accepted protected draft after navigation away and remount before settlement", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-click").click();
  await page.getByTestId("composer-input").fill("Deferred protected draft");
  await page.evaluate(() =>
    (
      window as unknown as { __mockDeferNext: (command: string) => void }
    ).__mockDeferNext("owner_enqueue_text"),
  );
  await page.getByTestId("composer-send").click();
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("composer-input")).toHaveValue(
    "Deferred protected draft",
  );
  await expect(page.getByTestId("composer-send")).toBeDisabled();
  await page.evaluate(() =>
    (
      window as unknown as { __mockReleaseDeferred: () => void }
    ).__mockReleaseDeferred(),
  );
  await expect(page.getByTestId("composer-input")).toBeEmpty();
  await expect(page.getByTestId("composer")).not.toContainText(
    "Text & images · hides after 30 seconds",
  );
  await page.getByTestId("composer-input").fill("Next ordinary text");
  await expect(page.getByTestId("composer-send")).toBeEnabled();
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByRole("log").getByText("Next ordinary text", { exact: true }),
  ).toBeVisible();
});

test("protected pasted image reveals through a local revocable URL and expires without export", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("composer-protection").click();
  await page.getByTestId("composer-protection-click").click();
  await page.evaluate(() => {
    const target = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (command: string, args?: unknown) => Promise<unknown>;
      };
      __protectedReadCalls: string[];
    };
    target.__protectedReadCalls = [];
    const invoke = target.__TAURI_INTERNALS__.invoke;
    target.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === "read_media" || command === "read_file")
        target.__protectedReadCalls.push(command);
      return invoke(command, args);
    };
  });
  await page.getByTestId("composer-input").evaluate((input) => {
    const canvas = document.createElement("canvas");
    canvas.width = 100;
    canvas.height = 60;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "#227866";
    context.fillRect(0, 0, 100, 60);
    const bytes = Uint8Array.from(
      atob(canvas.toDataURL("image/png").split(",")[1]),
      (char) => char.charCodeAt(0),
    );
    const transfer = new DataTransfer();
    transfer.items.add(new File([bytes], "private.png", { type: "image/png" }));
    input.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });
  await expect(page.getByTestId("composer-pending-image")).toBeVisible();
  await page.getByTestId("composer-send").click();
  const bubble = page.getByTestId("protected-message");
  await expect(bubble).toBeVisible();
  await expect(bubble.locator("img")).toHaveCount(0);
  await page.clock.install();
  await bubble.getByRole("button", { name: "Click to view" }).click();
  await expect(
    bubble.getByRole("img", { name: "Protected image" }),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/protected-image-visible.png" });
  const url = await bubble.getByRole("img").getAttribute("src");
  expect(url).toMatch(/^blob:/);
  await page.clock.fastForward(30000);
  await expect(bubble.locator("img")).toHaveCount(0);
  expect(
    await page.evaluate(async (url) => {
      try {
        await fetch(url!);
        return false;
      } catch {
        return true;
      }
    }, url),
  ).toBe(true);
  await bubble.getByRole("button", { name: "Click to view" }).click();
  await expect(bubble.getByRole("img")).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __protectedReadCalls: string[] })
          .__protectedReadCalls,
    ),
  ).toEqual([]);
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await expect(page.getByTestId("protected-message-body")).toHaveCount(0);
});

test("protected text and images are excluded from Favorites menus and selection snapshots", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  const selectProtection = async () => {
    await page.getByTestId("composer-protection").click();
    await page.getByTestId("composer-protection-click").click();
  };
  await selectProtection();
  await page
    .getByTestId("composer-input")
    .fill("Private favorite snapshot content");
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("protected-message")).toHaveCount(1);
  await selectProtection();
  await page.getByTestId("composer-input").evaluate((input) => {
    const canvas = document.createElement("canvas");
    canvas.width = 10;
    canvas.height = 10;
    const bytes = Uint8Array.from(
      atob(canvas.toDataURL("image/png").split(",")[1]),
      (char) => char.charCodeAt(0),
    );
    const transfer = new DataTransfer();
    transfer.items.add(
      new File([bytes], "private-favorite-filename.png", { type: "image/png" }),
    );
    input.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("protected-message")).toHaveCount(2);
  for (let index = 0; index < 2; index++) {
    await page
      .getByTestId("protected-message")
      .nth(index)
      .click({ button: "right" });
    await expect(page.getByTestId("message-context-menu")).toBeVisible();
    await expect(page.getByTestId("msg-collect")).toHaveCount(0);
    await expect(page.getByTestId("msg-select-messages")).toHaveCount(0);
    await page.keyboard.press("Escape");
  }
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-select-messages").click();
  await expect(
    page.getByRole("log").locator('input[type="checkbox"]:disabled'),
  ).toHaveCount(2);
  await expect(page.getByRole("log")).not.toContainText(
    "Private favorite snapshot content",
  );
  await expect(page.getByRole("log")).not.toContainText(
    "private-favorite-filename.png",
  );
  await page
    .getByTestId("favorite-selection-toolbar")
    .getByRole("button", { name: "Collect", exact: true })
    .click();
  await expect(page.getByTestId("favorite-editor")).toContainText(
    "hey, welcome to the mesh",
  );
  await expect(page.getByTestId("favorite-editor")).not.toContainText(
    "Private favorite snapshot content",
  );
  await expect(page.getByTestId("favorite-editor")).not.toContainText(
    "private-favorite-filename.png",
  );
});
