import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

const CHANNEL = "chan_team_dddd4444";
const CONVERSATION = `ch:${CHANNEL}`;
const BOB = "device_bob_2222";

// Self-contained because Page.evaluate serializes this function into the browser.
async function waitForInitialHistory({
  id,
  text,
  deadlineEpochMs,
}: {
  id: string;
  text: string;
  deadlineEpochMs: number;
}) {
  // Count caller-to-browser dispatch against the original assertion budget,
  // then use monotonic time so later wall-clock changes cannot extend it.
  const remaining = Math.min(5000, deadlineEpochMs - Date.now());
  const deadline = performance.now() + remaining;
  const expired = () => {
    if (performance.now() >= deadline)
      throw new Error(`Initial history readiness exceeded 5000ms: ${id}`);
  };
  await new Promise<void>((resolve, reject) => {
    let frame = 0;
    const visible = (element: Element | null): element is HTMLElement => {
      if (!(element instanceof HTMLElement)) return false;
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return (
        rect.width > 0 &&
        rect.height > 0 &&
        style.visibility === "visible" &&
        style.display !== "none"
      );
    };
    const cleanup = () => {
      observer.disconnect();
      clearTimeout(timer);
      cancelAnimationFrame(frame);
    };
    const schedule = () => {
      if (!frame)
        frame = requestAnimationFrame(() => {
          frame = 0;
          check();
        });
    };
    const check = () => {
      try {
        expired();
        const log = document.querySelector('[role="log"]');
        const row = log?.querySelector(`[data-message-id="${id}"]`) ?? null;
        const ready =
          visible(log) &&
          visible(row) &&
          [...row.querySelectorAll("*")].some(
            (element) => element.textContent === text && visible(element),
          );
        expired();
        if (ready) {
          cleanup();
          resolve();
        } else schedule();
      } catch (error) {
        cleanup();
        reject(error);
      }
    };
    const observer = new MutationObserver(check);
    const timer = setTimeout(
      () => {
        cleanup();
        reject(new Error(`Initial history readiness exceeded 5000ms: ${id}`));
      },
      Math.max(0, deadline - performance.now()),
    );
    observer.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      characterData: true,
    });
    check();
  });
}

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
  await page.evaluate(waitForInitialHistory, {
    id: `history-${count - 1}`,
    text: `History row ${count - 1}`,
    deadlineEpochMs: Date.now() + 5000,
  });
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

test("initial history rejects an already expired caller deadline", async ({
  page,
}) => {
  await page.setContent(
    '<div role="log"><div data-message-id="history-119"><span>History row 119</span></div></div>',
  );
  const target = {
    id: "history-119",
    text: "History row 119",
    deadlineEpochMs: Date.now() - 1,
  };
  await expect(page.evaluate(waitForInitialHistory, target)).rejects.toThrow(
    "Initial history readiness exceeded 5000ms: history-119",
  );
});

test("initial history rejects readiness sampled after its deadline", async ({
  page,
}) => {
  await page.setContent(
    '<div role="log"><div data-message-id="history-119"><span>History row 119</span></div></div>',
  );
  await page.evaluate(() => {
    const now = performance.now.bind(performance);
    const rect = Element.prototype.getBoundingClientRect;
    let offset = 0;
    Object.defineProperty(performance, "now", {
      configurable: true,
      value: () => now() + offset,
    });
    Element.prototype.getBoundingClientRect = function () {
      const value = rect.call(this);
      if (this.getAttribute("role") === "log") offset = 5001;
      return value;
    };
    (window as unknown as Record<string, unknown>).__restoreDeadlineClock =
      () => {
        delete (performance as unknown as Record<string, unknown>).now;
        Element.prototype.getBoundingClientRect = rect;
      };
  });
  try {
    await expect(
      page.evaluate(waitForInitialHistory, {
        id: "history-119",
        text: "History row 119",
        deadlineEpochMs: Date.now() + 5000,
      }),
    ).rejects.toThrow("Initial history readiness exceeded 5000ms: history-119");
  } finally {
    await page.evaluate(() =>
      (
        (window as unknown as Record<string, unknown>)
          .__restoreDeadlineClock as () => void
      )(),
    );
  }
});

test("initial history readiness rejects absent and hidden rows and recovers", async ({
  page,
}) => {
  await page.setContent('<div role="log"></div>');
  await page.evaluate(() => {
    const observers = new Set<MutationObserver>();
    const frames = new Set<number>();
    const NativeObserver = window.MutationObserver;
    const request = window.requestAnimationFrame.bind(window);
    const cancel = window.cancelAnimationFrame.bind(window);
    window.MutationObserver = class extends NativeObserver {
      override observe(target: Node, options?: MutationObserverInit) {
        super.observe(target, options);
        observers.add(this);
        if (
          (window as unknown as Record<string, unknown>).__recoverInitialHistory
        )
          queueMicrotask(() => {
            document.querySelector<HTMLElement>(
              '[data-message-id="history-119"]',
            )!.hidden = false;
          });
      }
      override disconnect() {
        super.disconnect();
        observers.delete(this);
      }
    };
    let heldFrame = 0;
    window.requestAnimationFrame = (callback) => {
      if (
        (window as unknown as Record<string, unknown>).__recoverInitialHistory
      ) {
        const id = --heldFrame;
        frames.add(id);
        return id;
      }
      const id = request((time) => {
        frames.delete(id);
        callback(time);
      });
      frames.add(id);
      return id;
    };
    window.cancelAnimationFrame = (id) => {
      frames.delete(id);
      cancel(id);
    };
    (window as unknown as Record<string, unknown>).__historyReadiness = {
      observers,
      frames,
    };
  });
  const target = { id: "history-119", text: "History row 119" };
  const pending = () =>
    page.evaluate(() => {
      const state = (
        window as unknown as {
          __historyReadiness: {
            observers: Set<MutationObserver>;
            frames: Set<number>;
          };
        }
      ).__historyReadiness;
      return { observers: state.observers.size, frames: state.frames.size };
    });
  await expect(
    page.evaluate(waitForInitialHistory, {
      ...target,
      deadlineEpochMs: Date.now() + 5000,
    }),
  ).rejects.toThrow("Initial history readiness exceeded 5000ms: history-119");
  expect(await pending()).toEqual({ observers: 0, frames: 0 });
  await page.evaluate(() => {
    document.querySelector('[role="log"]')!.innerHTML =
      '<div data-message-id="history-119" hidden><span>History row 119</span></div>';
  });
  await expect(
    page.evaluate(waitForInitialHistory, {
      ...target,
      deadlineEpochMs: Date.now() + 5000,
    }),
  ).rejects.toThrow("Initial history readiness exceeded 5000ms: history-119");
  expect(await pending()).toEqual({ observers: 0, frames: 0 });
  await page.evaluate(() => {
    // Commit recovery only after the observer is registered, without a timer race.
    (window as unknown as Record<string, unknown>).__recoverInitialHistory =
      true;
  });
  await page.evaluate(waitForInitialHistory, {
    ...target,
    deadlineEpochMs: Date.now() + 5000,
  });
  expect(await pending()).toEqual({ observers: 0, frames: 0 });
});

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
      // The poll owns readiness; selector waiting would consume its budget
      // before a geometry sample can complete on a traced desktop runner.
      const visible = await page.evaluate(() => {
        const element = document.querySelector<HTMLElement>('[role="log"]');
        if (!element) return null;
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
      page.evaluate((id) => {
        const log = document.querySelector<HTMLElement>('[role="log"]');
        const row =
          log &&
          [...log.querySelectorAll<HTMLElement>("[data-message-id]")].find(
            (candidate) => candidate.dataset.messageId === id,
          );
        return row && log
          ? row.getBoundingClientRect().top - log.getBoundingClientRect().top
          : null;
      }, anchor.id),
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
  // A partial delivery and its terminal delivery exercise the batch boundary.
  // Larger history windows and announcement coalescing have dedicated controls.
  await receiveBatch(page, [
    { id: "Recovered batch first", index: 1, complete: false },
    { id: "Recovered batch last", index: 1, complete: true },
  ]);
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
    "3 unread messages",
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
        text: "Recovered batch last",
        reply_to: null,
        event_id: "Recovered batch last",
        arrival_kind: "catch_up",
        batch_id: "recovery-1",
        batch_complete: true,
      });
    },
    { channel: CHANNEL, who: BOB },
  );
  await expect(page.getByTestId("reception-announcement")).toHaveText(
    "3 unread messages",
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
