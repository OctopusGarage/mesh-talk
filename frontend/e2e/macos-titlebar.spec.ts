import { expect, test } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test.describe("macOS overlay titlebar", () => {
  test.use({
    userAgent:
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
  });

  test("sign-in and registration explain where Touch ID is configured", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 760, height: 520 });
    await page.goto("/");
    const guidance = page.getByTestId("mac-auth-guidance");
    await expect(guidance).toContainText("macOS System Settings");
    await expect(page.getByTestId("login-submit")).toBeInViewport();
    await page.getByTestId("login-tab-register").click();
    await expect(guidance).toBeVisible();
    await expect(page.getByTestId("login-submit")).toBeInViewport();
  });

  for (const viewport of [
    { width: 760, height: 520 },
    { width: 1040, height: 720 },
  ]) {
    test(`profile clears traffic lights at ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await page.setViewportSize(viewport);
      await enterChat(page);
      await expect(page.locator("html")).toHaveAttribute("data-os", "macos");
      const avatar = page.getByTestId("open-profile");
      await expect(page.getByTestId("self-identity")).toHaveCSS(
        "padding-top",
        "28px",
      );
      const bounds = await avatar.boundingBox();
      expect(bounds).not.toBeNull();
      expect(bounds!.y).toBeGreaterThanOrEqual(28);
      // Check the avatar's top edge, not just its center: the old layout's upper
      // portion was covered by both traffic lights and the invisible drag strip.
      expect(
        await avatar.evaluate((element) => {
          const rect = element.getBoundingClientRect();
          return element.contains(
            document.elementFromPoint(rect.x + rect.width / 2, rect.y + 1),
          );
        }),
      ).toBe(true);
      await avatar.click({ position: { x: 19, y: 1 } });
      await expect(page.getByRole("dialog")).toBeVisible();
      await page.keyboard.press("Escape");
      await openBobDm(page);
      await expect(page.getByTestId("conversation-header")).toHaveCSS(
        "padding-top",
        "28px",
      );
    });
  }

  test("lock uses macOS authentication without asking for the app password", async ({
    page,
  }) => {
    await enterChat(page);
    await page.evaluate(() => {
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args?: Record<string, unknown>,
            ) => Promise<unknown>;
          };
          __unlockArgs?: Record<string, unknown>;
        }
      ).__TAURI_INTERNALS__;
      const original = internals.invoke;
      internals.invoke = (command, args) => {
        if (command === "unlock_app") {
          (
            window as unknown as { __unlockArgs?: Record<string, unknown> }
          ).__unlockArgs = args;
          return new Promise(() => {});
        }
        return original(command, args);
      };
    });

    await openSidebarMenuAction(page, "sidebar-lock");
    await expect(page.getByTestId("lock-screen")).toBeVisible();
    await expect(page.getByPlaceholder("Password")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Use Touch ID or Mac password" }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __unlockArgs?: { password: unknown } })
              .__unlockArgs?.password,
        ),
      )
      .toBeNull();
  });
});

for (const [platform, userAgent] of [
  ["Windows", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chromium/120"],
  ["Linux", "Mozilla/5.0 (X11; Linux x86_64) Chromium/120"],
]) {
  test.describe(`${platform} sidebar`, () => {
    test.use({ userAgent });
    test("leaves a drag band above sidebar actions", async ({ page }) => {
      await enterChat(page);
      await expect(page.getByTestId("self-identity")).toHaveCSS(
        "padding-top",
        "40px",
      );
      await page.getByTestId("open-profile").click();
      await expect(page.getByRole("dialog")).toBeVisible();
    });
  });
}
