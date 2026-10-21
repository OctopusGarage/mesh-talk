import { expect, test } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";
import type { PresenceMap } from "../src/lib/types";

const BOB = "acc_bob_bbbb2222";
const GROUP = "chan_team_dddd4444";

test("group header counts accepted people and each row has its own state", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${GROUP}`).click();
  await expect(page.getByTestId("conversation-header")).toContainText(
    "3 members · 2 known online",
  );
  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByTestId("member-profile-device_bob_2222"),
  ).toContainText("Online");
  await expect(
    dialog.getByTestId("member-profile-device_carol_3333"),
  ).toContainText("Offline");
});

for (const variant of [
  { theme: "dark", width: 1280, height: 800 },
  { theme: "light", width: 760, height: 520 },
  { theme: "cat-acrylic", width: 1280, height: 800 },
]) {
  test(`presence warning stays in the footer with readable details: ${variant.theme}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({
      width: variant.width,
      height: variant.height,
    });
    await seedThemeBeforeLoad(page, variant.theme);
    await enterChat(page);
    await openBobDm(page);
    const header = page.getByTestId("conversation-header");
    const before = await header.boundingBox();
    await page.evaluate(() => {
      const host = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (command: string, args: unknown) => Promise<unknown>;
        };
        __mockPresenceFailure: (failed: boolean) => void;
      };
      const original = host.__TAURI_INTERNALS__.invoke;
      host.__TAURI_INTERNALS__.invoke = async (command, args) => {
        try {
          return await original(command, args);
        } catch (error) {
          if (command === "get_presence")
            throw {
              kind: "io",
              message: "snapshot read failed: " + "endpoint".repeat(100),
            };
          throw error;
        }
      };
      host.__mockPresenceFailure(true);
    });
    const notice = page
      .getByTestId("sidebar")
      .getByTestId("presence-availability");
    await expect(notice).toBeVisible();
    await expect(notice.getByRole("status")).toHaveText(
      "Online status unavailable. Retrying automatically.",
    );
    const summary = notice.locator("summary");
    const detail = notice.getByTestId("presence-failure-detail");
    await expect(detail).toBeHidden();
    await page.screenshot({
      path: testInfo.outputPath("presence-collapsed.png"),
    });
    await summary.focus();
    await page.keyboard.press("Enter");
    await expect(detail).toContainText("snapshot read failed:");
    await expect(detail).toBeVisible();
    expect(await header.boundingBox()).toEqual(before);
    const bounds = await notice.evaluate((element) => {
      const box = element.getBoundingClientRect();
      const sidebar = element
        .closest('[data-testid="sidebar"]')!
        .getBoundingClientRect();
      return {
        overflow: element.scrollWidth - element.clientWidth,
        left: box.left - sidebar.left,
        right: box.right - sidebar.right,
        bottom: box.bottom,
      };
    });
    expect(bounds.overflow).toBeLessThanOrEqual(1);
    expect(bounds.left).toBeGreaterThanOrEqual(0);
    expect(bounds.right).toBeLessThanOrEqual(0);
    expect(bounds.bottom).toBeLessThanOrEqual(variant.height);
    await expect(page.getByTestId("composer-input")).toBeVisible();
    await page.screenshot({
      path: testInfo.outputPath("presence-expanded.png"),
    });
    await page.keyboard.press("Space");
    await expect(detail).toBeHidden();
    await page.evaluate(() => {
      (
        window as unknown as {
          __mockPresenceFailure: (failed: boolean) => void;
        }
      ).__mockPresenceFailure(false);
    });
    await expect(notice).toBeHidden();
    await page.evaluate(() => {
      (
        window as unknown as {
          __mockPresenceFailure: (failed: boolean) => void;
        }
      ).__mockPresenceFailure(true);
    });
    await expect(notice).toBeVisible();
    await expect(notice).not.toHaveAttribute("open", "");
  });
}

test("profile deduplicates a device with overlapping discovery sources", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        last_seen_secs: 0,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "local", last_seen_secs: 0, expires_in_secs: 30 },
              { source: "routed", last_seen_secs: 0, expires_in_secs: 120 },
              { source: "directory", last_seen_secs: 0, expires_in_secs: 10 },
            ],
          },
        ],
      },
    });
  }, BOB);
  await page.getByTestId("conversation-member-profile").click();
  const details = page.getByTestId("presence-device-details");
  await expect(details).toContainText("Freshly observed devices: 1");
  for (const label of [
    "Local network discovery",
    "Cross-network discovery",
    "Discovered through a post office",
  ])
    await expect(details).toContainText(label);
  await expect(page.getByRole("dialog")).toContainText(
    "Offline means this client",
  );
});

test("failed refresh announces unavailability, expires evidence, and recovers", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        last_seen_secs: 0,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "directory", last_seen_secs: 0, expires_in_secs: 4 },
            ],
          },
        ],
      },
    });
  }, BOB);
  // Let the shared two-second poll consume this short origin lease before failure.
  await page.waitForTimeout(2200);
  await page.evaluate(() => {
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(true);
  });
  await expect(
    page.getByRole("status").filter({ hasText: "Online status unavailable" }),
  ).toBeVisible();
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Offline",
    { timeout: 10000 },
  );
  await page.evaluate(() => {
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(false);
  });
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(
    page.getByRole("status").filter({ hasText: "Online status unavailable" }),
  ).toBeHidden();
});
