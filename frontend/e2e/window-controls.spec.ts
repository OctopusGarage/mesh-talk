import { test, expect } from "./tauri-mock";
import { CHANNEL, enterChat, openBobDm } from "./helpers/session";

// Frameless window (Windows/Linux): the native title bar is dropped, so the app draws its
// own min/max/close. Force a non-Mac UA so needsCustomWindowControls() is true here (the CI
// runner is macOS, where they're intentionally hidden in favor of the native traffic-lights).
test.use({
  userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chromium/120",
});

test("frameless window shows custom min/max/close controls", async ({
  page,
}) => {
  await page.goto("/");
  for (const ctl of ["Minimize", "Maximize", "Close"]) {
    await expect(page.getByRole("button", { name: ctl })).toBeVisible();
  }
});

test("empty chat has a usable window drag band", async ({ page }) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enterChat(page);
  const region = page.getByTestId("empty-chat-drag-region");
  await expect(region).toBeVisible();
  const bounds = await region.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.height).toBeGreaterThanOrEqual(32);
  expect(bounds!.width).toBeGreaterThan(200);
  expect(
    await region.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return element === document.elementFromPoint(rect.x + rect.width / 2, 16);
    }),
  ).toBe(true);
});

test("conversation actions clear the window controls", async ({ page }) => {
  for (const [width, conversation] of [
    [760, "dm"],
    [1040, "channel"],
  ] as const) {
    await page.setViewportSize({ width, height: 720 });
    await enterChat(page);
    if (conversation === "dm") {
      await openBobDm(page);
    } else {
      await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    }
    const controls = page.getByRole("button", { name: "Close" });
    const header = page.getByTestId("conversation-header");
    const action = header.getByRole("button").last();
    const controlBounds = await controls.boundingBox();
    const actionBounds = await action.boundingBox();
    expect(controlBounds).not.toBeNull();
    expect(actionBounds).not.toBeNull();
    expect(actionBounds!.y).toBeGreaterThanOrEqual(
      controlBounds!.y + controlBounds!.height,
    );
    expect(actionBounds!.x + actionBounds!.width).toBeLessThanOrEqual(width);
    expect(
      await header.evaluate((element) => {
        const rect = element.getBoundingClientRect();
        return (
          element === document.elementFromPoint(rect.x + rect.width / 2, 16)
        );
      }),
    ).toBe(true);
  }
});
