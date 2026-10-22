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
      "Presence updates unavailable",
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

test("profile names introducing offices alongside local multicast and expires sources during refresh failure", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 760, height: 520 });
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
              { source: "local", last_seen_secs: 0, expires_in_secs: 4 },
              {
                source: "directory",
                last_seen_secs: 0,
                expires_in_secs: 30,
                post_office: {
                  user_id: "office-mini",
                  name: "mac-mini-post-office",
                  endpoint: "100.109.255.49:47479",
                },
              },
              {
                source: "directory",
                last_seen_secs: 0,
                expires_in_secs: 30,
                post_office: {
                  user_id: "office-2015",
                  name: "mac2015-post-office",
                  endpoint: "192.168.3.72:47479",
                },
              },
            ],
          },
        ],
      },
    });
  }, BOB);
  await page.getByTestId("conversation-member-profile").click();
  const sources = page.getByTestId("member-discovery-sources");
  await expect(sources).toContainText("Local LAN · UDP multicast");
  await expect(sources).toContainText("Post office · mac-mini-post-office");
  await expect(sources).toContainText("Post office · mac2015-post-office");
  await expect(sources.getByRole("listitem")).toHaveCount(3);
  await sources.scrollIntoViewIfNeeded();
  await page.screenshot({
    path: testInfo.outputPath("member-discovery-sources.png"),
  });
  await page.evaluate(() => {
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(true);
  });
  await expect(sources).not.toContainText("Local LAN · UDP multicast", {
    timeout: 10000,
  });
  await expect(sources.getByRole("listitem")).toHaveCount(2);
  await expect(sources).toContainText("mac-mini-post-office");
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
    page
      .getByRole("status")
      .filter({ hasText: "Presence updates unavailable" }),
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
    page
      .getByRole("status")
      .filter({ hasText: "Presence updates unavailable" }),
  ).toBeHidden();
});

test("expired presence reconnects without green status or an offline prompt, then reaches its fixed bound", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: false,
        last_seen_secs: 20,
        reconnect_expires_in_secs: 10,
        devices: [],
      },
    });
  }, BOB);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Slow connection",
  );
  await expect(page.getByTestId("connection-status")).toContainText(
    "Slow connection",
  );
  await expect(
    page.getByTestId(`contact-profile-${BOB}`).getByRole("status"),
  ).toHaveAttribute("aria-label", "Slow connection");
  await expect(page.getByTestId("stranded-prompt")).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(true);
  });
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Offline",
    { timeout: 12000 },
  );
  await page.evaluate((account) => {
    const mock = window as unknown as {
      __mockSetPresence: (map: PresenceMap) => void;
      __mockPresenceFailure: (failed: boolean) => void;
    };
    mock.__mockSetPresence({
      [account]: {
        online: true,
        last_seen_secs: 0,
        reconnect_expires_in_secs: 30,
      },
    });
    mock.__mockPresenceFailure(false);
  }, BOB);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(page.getByTestId("connection-status")).toContainText(
    "Ready to find people",
  );
});

test("cross-network presence stays online for two minutes with one concise quality label", async ({
  page,
}) => {
  await page.clock.install();
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        connection_weak: true,
        last_seen_secs: 0,
        reconnect_expires_in_secs: 120,
        online_expires_in_secs: 120,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "routed", last_seen_secs: 0, expires_in_secs: 15 },
            ],
          },
        ],
      },
    });
  }, BOB);
  await page.clock.runFor(2100);
  await page.evaluate(() => {
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(true);
  });
  // Exercise elapsed-time expiry without replaying every background poll.
  await page.clock.fastForward(31_000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(page.getByTestId("connection-status")).not.toContainText(
    "Searching",
  );
  await expect(page.getByTestId("stranded-prompt")).toHaveCount(0);
  await page.clock.fastForward(86_000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(page.getByTestId("stranded-prompt")).toHaveCount(0);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online · slow connection",
  );
  await expect(page.getByTestId("connection-quality")).toHaveCount(0);
  await expect(
    page.getByText("Messages and delivery confirmations may take longer", {
      exact: false,
    }),
  ).toHaveCount(0);
  await page.screenshot({ path: "test-results/cross-network-two-minutes.png" });
  await page.clock.runFor(4000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Offline",
  );
});

test("pure LAN presence expires promptly without a long network-quality notice", async ({
  page,
}) => {
  await page.clock.install();
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        connection_weak: false,
        last_seen_secs: 0,
        online_expires_in_secs: 15,
        reconnect_expires_in_secs: 15,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "local", last_seen_secs: 0, expires_in_secs: 15 },
            ],
          },
        ],
      },
    });
  }, BOB);
  await page.clock.runFor(2100);
  await page.evaluate(() =>
    (
      window as unknown as { __mockPresenceFailure: (failed: boolean) => void }
    ).__mockPresenceFailure(true),
  );
  await page.clock.runFor(13_000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await page.clock.runFor(2100);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Offline",
  );
  await expect(page.getByTestId("connection-quality")).toHaveCount(0);
});

test("a fresh LAN observation clears the routed slow hint without a thirty-second wait", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  const profile = page.getByTestId("conversation-member-profile");
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        connection_weak: true,
        last_seen_secs: 20,
        online_expires_in_secs: 100,
        reconnect_expires_in_secs: 100,
        devices: [],
      },
    });
  }, BOB);
  await expect(profile).toContainText("Online · slow connection");
  await page.evaluate((account) => {
    (
      window as unknown as { __mockSetPresence: (map: PresenceMap) => void }
    ).__mockSetPresence({
      [account]: {
        online: true,
        connection_weak: false,
        last_seen_secs: 0,
        online_expires_in_secs: 120,
        reconnect_expires_in_secs: 120,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "local", last_seen_secs: 0, expires_in_secs: 15 },
            ],
          },
        ],
      },
    });
  }, BOB);
  await expect(profile).not.toContainText("slow connection", { timeout: 5000 });
  await expect(profile).toContainText("Online");
});
