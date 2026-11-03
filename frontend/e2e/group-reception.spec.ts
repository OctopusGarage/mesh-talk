import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

const CHANNEL = "chan_team_dddd4444";
const CONVERSATION = `ch:${CHANNEL}`;
const BOB = "device_bob_2222";

async function enterGroup(page: Page, count = 120) {
  await page.goto("/?data=group-reception-window");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
  // Persist initial history before opening it: no notification/read shortcut.
  await page.evaluate(
    ({ conversation, who, count }) => {
      const inject = (
        window as unknown as {
          __mockInject: (
            c: string,
            t: string,
            who: string,
            m?: { id: string },
          ) => void;
        }
      ).__mockInject;
      for (let i = 0; i < count; i++)
        inject(conversation, `History row ${i}`, who, { id: `history-${i}` });
    },
    { conversation: CONVERSATION, who: BOB, count },
  );
  await page.getByTestId(`conversation-row-${CHANNEL}`).click();
  await expect(
    page
      .getByRole("log")
      .getByText(`History row ${count - 1}`, { exact: true }),
  ).toBeVisible();
}

type RecoveryDelivery = { id: string; index: number; complete: boolean };

async function receiveBatch(page: Page, deliveries: RecoveryDelivery[]) {
  await page.evaluate(
    async ({ conversation, who, channel, deliveries }) => {
      const w = window as unknown as {
        __mockInject: (
          c: string,
          t: string,
          who: string,
          m: { id: string; index: number; wall_clock: number },
        ) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      for (const [position, { id, index, complete }] of deliveries.entries()) {
        w.__mockInject(conversation, id, who, { id, index, wall_clock: 1 });
        w.__mockEmit("channel-message", {
          channel_id: channel,
          channel_name: "team",
          from: who,
          from_name: "bob",
          text: id,
          reply_to: null,
          event_id: id,
          authored_at: 1,
          received_at: Date.now(),
          arrival_kind: "catch_up",
          batch_id: "recovery-1",
          batch_complete: complete,
        });
        if (position < deliveries.length - 1) {
          // Let each delivery render before the next event, while avoiding a
          // controller round trip (and its trace snapshots) for every message.
          await new Promise<void>((resolve) => {
            requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
          });
        }
      }
    },
    {
      conversation: CONVERSATION,
      who: BOB,
      channel: CHANNEL,
      deliveries,
    },
  );
}

async function receive(page: Page, id: string, index: number, complete = true) {
  await receiveBatch(page, [{ id, index, complete }]);
}

test("late history preserves reading anchor/focus and stays unread until viewed", async ({
  page,
}) => {
  await enterGroup(page);
  const log = page.getByRole("log");
  await log.dispatchEvent("wheel", { deltaY: -1 });
  await log.evaluate((element) => {
    element.scrollTop = element.scrollHeight / 2;
    element.dispatchEvent(new Event("scroll"));
  });
  await expect(page.getByTestId("jump-to-latest")).toBeVisible();
  let anchor = { id: "", offset: 0 };
  await expect
    .poll(async () => {
      const visible = await log.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        const row = [
          ...element.querySelectorAll<HTMLElement>("[data-message-id]"),
        ].find((candidate) => {
          const rect = candidate.getBoundingClientRect();
          return rect.bottom > bounds.top && rect.top < bounds.bottom;
        });
        return row
          ? {
              id: row.dataset.messageId!,
              offset: row.getBoundingClientRect().top - bounds.top,
            }
          : null;
      });
      if (visible) anchor = visible;
      return visible !== null;
    })
    .toBe(true);
  await page.getByTestId("composer-input").focus();
  await receive(page, "Recovered earlier row", 1, false);
  await expect(page.getByTestId("earlier-messages")).toBeVisible();
  await expect(page.getByTestId("composer-input")).toBeFocused();
  await expect
    .poll(async () =>
      page
        .locator(`[data-message-id="${anchor.id}"]`)
        .evaluate(
          (row) =>
            row.getBoundingClientRect().top -
            row.closest('[role="log"]')!.getBoundingClientRect().top,
        ),
    )
    .toBeCloseTo(anchor.offset, 0);
  // Clicking/focusing the composer cannot mark the offscreen insertion read.
  await page.getByTestId("composer-input").click();
  await expect(page.getByTestId("earlier-messages")).toBeVisible();
  await page.getByTestId("earlier-messages").click();
  await expect(
    page.getByText("Recovered earlier row", { exact: true }),
  ).toBeVisible();
  await expect(page.getByTestId("earlier-messages")).toHaveCount(0);
  await expect(page.getByTestId("delayed-message")).toBeVisible();
  await expect(log).toHaveAttribute("aria-live", "off");
});

test("recovery batches remain discoverable alongside live messages without duplicate announcements", async ({
  page,
}) => {
  await enterGroup(page);
  const log = page.getByRole("log");
  await log.dispatchEvent("wheel", { deltaY: -1 });
  await log.evaluate((element) => {
    element.scrollTop = element.scrollHeight / 2;
    element.dispatchEvent(new Event("scroll"));
  });
  await expect(page.getByTestId("jump-to-latest")).toBeVisible();
  await page.getByTestId("composer-input").focus();
  await receiveBatch(
    page,
    Array.from({ length: 8 }, (_, i) => ({
      id: `Recovered batch ${i}`,
      index: 1,
      complete: i === 7,
    })),
  );
  await expect(page.getByTestId("earlier-messages")).toBeVisible();
  await page.evaluate(
    ({ channel, conversation, who }) => {
      const w = window as unknown as {
        __mockInject: (
          c: string,
          text: string,
          who: string,
          metadata: { id: string },
        ) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      w.__mockInject(conversation, "Concurrent live message", who, {
        id: "concurrent-live",
      });
      w.__mockEmit("channel-message", {
        channel_id: channel,
        channel_name: "team",
        from: who,
        text: "Concurrent live message",
        reply_to: null,
        event_id: "concurrent-live",
        arrival_kind: "live",
      });
    },
    { channel: CHANNEL, conversation: CONVERSATION, who: BOB },
  );
  await expect(page.getByTestId("new-messages")).toBeVisible();
  await expect(page.getByTestId("reception-announcement")).toHaveText(
    "9 unread messages",
  );
  // Duplicate transport signal must leave the summary, row count and navigation intact.
  await page.evaluate(
    ({ channel, who }) => {
      (
        window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        }
      ).__mockEmit("channel-message", {
        channel_id: channel,
        channel_name: "team",
        from: who,
        text: "Recovered batch 7",
        reply_to: null,
        event_id: "Recovered batch 7",
        arrival_kind: "catch_up",
        batch_id: "recovery-1",
        batch_complete: true,
      });
    },
    { channel: CHANNEL, who: BOB },
  );
  await expect(page.getByTestId("reception-announcement")).toHaveText(
    "9 unread messages",
  );
  await expect(page.getByTestId("composer-input")).toBeFocused();
  await page.getByTestId("new-messages").click();
  await expect(
    log.getByText("Concurrent live message", { exact: true }),
  ).toBeVisible();
  await expect(page.getByTestId("new-messages")).toHaveCount(0);
  await expect(page.getByTestId("earlier-messages")).toBeVisible();
});

test("unread navigation expands the loaded history window before revealing an older insertion", async ({
  page,
}) => {
  await enterGroup(page, 320);
  await receive(page, "Recovered beyond initial window", 1);
  await expect(page.getByTestId("earlier-messages")).toBeVisible();
  await expect(
    page.getByText("Recovered beyond initial window", { exact: true }),
  ).toHaveCount(0);
  await page.getByTestId("earlier-messages").click();
  await expect(
    page.getByText("Recovered beyond initial window", { exact: true }),
  ).toBeVisible();
  await expect(page.getByTestId("earlier-messages")).toHaveCount(0);
  await expect(page.getByTestId("unread-unavailable")).toHaveCount(0);
});

test("live messages follow the bottom and become read only after their row is displayed", async ({
  page,
}) => {
  await enterGroup(page);
  await page.getByTestId("composer-input").focus();
  await page.evaluate(
    ({ channel, conversation, who }) => {
      const w = window as unknown as {
        __mockInject: (
          c: string,
          text: string,
          who: string,
          metadata: { id: string },
        ) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      w.__mockInject(conversation, "Live at bottom", who, {
        id: "live-at-bottom",
      });
      w.__mockEmit("channel-message", {
        channel_id: channel,
        channel_name: "team",
        from: who,
        text: "Live at bottom",
        reply_to: null,
        event_id: "live-at-bottom",
        arrival_kind: "live",
      });
    },
    { channel: CHANNEL, conversation: CONVERSATION, who: BOB },
  );
  await expect(
    page.getByRole("log").getByText("Live at bottom", { exact: true }),
  ).toBeVisible();
  await expect(page.getByTestId("new-messages")).toHaveCount(0);
  await expect(page.getByTestId("jump-to-latest")).toHaveCount(0);
  await expect(page.getByTestId("composer-input")).toBeFocused();
});
