import { expect, test } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
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
    page
      .getByRole("status")
      .filter({ hasText: "Presence updates are unavailable" }),
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
      .filter({ hasText: "Presence updates are unavailable" }),
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
    "Weak connection",
  );
  await expect(page.getByTestId("connection-status")).toContainText(
    "Weak connection",
  );
  await expect(
    page.getByTestId(`contact-profile-${BOB}`).getByRole("status"),
  ).toHaveAttribute("aria-label", "Weak connection");
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

test("poor-network recovery stays visible for sixty seconds without restarting network search", async ({
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
        reconnect_expires_in_secs: 60,
        devices: [
          {
            user_id: "device_bob_2222",
            evidence: [
              { source: "routed", last_seen_secs: 0, expires_in_secs: 60 },
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
  await page.clock.runFor(31_000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(page.getByTestId("connection-status")).not.toContainText(
    "Searching",
  );
  await expect(page.getByTestId("stranded-prompt")).toHaveCount(0);
  await page.clock.runFor(26_000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Online",
  );
  await expect(page.getByTestId("stranded-prompt")).toHaveCount(0);
  await expect(page.getByTestId("connection-quality")).toContainText(
    "Messages and delivery confirmations may take longer",
  );
  await page.screenshot({ path: "test-results/recovery-sixty-seconds.png" });
  await page.clock.runFor(4000);
  await expect(page.getByTestId("conversation-member-profile")).toContainText(
    "Offline",
  );
});
