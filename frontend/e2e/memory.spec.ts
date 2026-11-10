import { runConversationChurn } from "./helpers/conversation-churn";
import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

// Stable scenario ids — mirror e2e/tauri-mock.ts.
const BOB = { account: "acc_bob_bbbb2222" };
const CAROL = { account: "acc_carol_cccc3333" };
const CHANNEL = { id: "chan_team_dddd4444" };

/**
 * Frontend memory-growth guardrail.
 *
 * Two invariants that a regression could silently break, each of which would make
 * the UI's memory footprint grow without bound:
 *
 *  1. Virtualization (react-virtuoso) keeps only the on-screen message bubbles in
 *     the DOM. If it were ever defeated (e.g. a layout change rendering the whole
 *     list), the bubble count would scale with history size. We inject hundreds of
 *     synthetic inbound messages and assert the rendered `[data-testid=message-bubble]`
 *     node count stays small regardless.
 *
 *  2. Per-conversation store isolation: rapidly switching conversations many times
 *     must not leak rendered bubbles from prior conversations into the DOM (the
 *     virtualized list is keyed per conversation and resets). After churning, the
 *     bubble count must still be bounded.
 *
 * Deterministic + fast: uses the mock fixture's `__mockInject` / `__mockEmit` hooks;
 * no real backend or timed message arrivals.
 */

async function enterChat(page: Page, user = "mem") {
  await page.goto("/");
  await page.getByTestId("login-username").fill(user);
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
  await expect(
    page.getByTestId(`conversation-row-${BOB.account}`),
  ).toBeVisible();
}

const bubbleCount = (page: Page) =>
  page.evaluate(
    () => document.querySelectorAll('[data-testid="message-bubble"]').length,
  );

// A virtualized viewport (plus react-virtuoso's overscan) renders only a handful
// of bubbles. Stay well under any plausible "whole history mounted" count.
const MAX_RENDERED = 60;

test.describe("Frontend memory-growth guardrails", () => {
  test("virtualization keeps DOM bubble count bounded as history grows", async ({
    page,
  }) => {
    await enterChat(page);
    await page.getByTestId(`conversation-row-${BOB.account}`).click();
    await expect(page.getByTestId("conversation-header")).toBeVisible();
    await expect(
      page.getByRole("log").getByText("hey, welcome to the mesh"),
    ).toBeVisible();

    // Inject 400 synthetic inbound messages into the open conversation's store,
    // then nudge the view to reload by emitting an inbound event for the last one.
    const N = 400;
    await page.evaluate((count) => {
      const w = window as unknown as Record<string, unknown>;
      const inject = w.__mockInject as (
        c: string,
        t: string,
        who: string,
      ) => void;
      for (let i = 0; i < count; i++) {
        inject(
          "acc:acc_bob_bbbb2222",
          `synthetic message ${i}`,
          "device_bob_2222",
        );
      }
      (w.__mockEmit as (e: string, p: unknown) => void)("dm-received", {
        from: "device_bob_2222",
        from_name: "bob",
        text: `synthetic message ${count - 1}`,
        reply_to: null,
      });
    }, N);

    // The newest message renders (list followed output to the bottom)...
    await expect(
      page.getByRole("log").getByText(`synthetic message ${N - 1}`),
    ).toBeVisible();
    // ...but the DOM holds only the on-screen window, NOT all N bubbles.
    const rendered = await bubbleCount(page);
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThan(MAX_RENDERED);
    expect(rendered).toBeLessThan(N);
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("committed visible destinations settle without waiting for a frame", async ({
    page,
  }) => {
    await page.setContent(`
      <button data-testid="conversation-row-bob">Bob</button>
      <button data-testid="conversation-row-channel">Channel</button>
      <button data-testid="conversation-row-carol">Carol</button>
      <div data-testid="conversation-header" hidden>Conversation</div>
      <div role="log"><span></span></div>
      <div data-testid="conversation-empty" hidden>Empty</div>
    `);
    await page.evaluate(() => {
      let frameId = 0;
      const frames = new Set<number>();
      const observers = new Set<MutationObserver>();
      const NativeObserver = window.MutationObserver;
      window.MutationObserver = class extends NativeObserver {
        override observe(target: Node, options?: MutationObserverInit) {
          super.observe(target, options);
          observers.add(this);
        }
        override disconnect() {
          super.disconnect();
          observers.delete(this);
        }
      };
      (window as unknown as Record<string, unknown>).__activeObservers =
        observers;
      window.requestAnimationFrame = () => {
        frames.add(++frameId);
        return frameId;
      };
      window.cancelAnimationFrame = (id) => {
        frames.delete(id);
      };
      (window as unknown as Record<string, unknown>).__pendingFrames = frames;
      for (const button of document.querySelectorAll("button")) {
        button.addEventListener("click", () =>
          queueMicrotask(() => {
            const id = button.dataset.testid?.replace("conversation-row-", "");
            for (const row of document.querySelectorAll("button"))
              row.setAttribute("aria-current", String(row === button));
            document.querySelector<HTMLElement>(
              '[data-testid="conversation-header"]',
            )!.hidden = id !== "bob";
            document.querySelector<HTMLElement>(
              '[data-testid="conversation-empty"]',
            )!.hidden = id !== "carol";
            document.querySelector('[role="log"] span')!.textContent =
              id === "bob"
                ? (window as unknown as Record<string, unknown>).__blockHistory
                  ? "pending"
                  : "history 299"
                : id === "channel"
                  ? "channel kickoff"
                  : "";
          }),
        );
      }
    });
    const result = await page.evaluate(runConversationChurn, {
      bob: "bob",
      channel: "channel",
      carol: "carol",
    });
    expect(result).toEqual({ rounds: 24, activations: 72 });
    const pending = () =>
      page.evaluate(() => {
        const state = window as unknown as Record<string, unknown>;
        return {
          frames: (state.__pendingFrames as Set<number>).size,
          observers: (state.__activeObservers as Set<MutationObserver>).size,
        };
      });
    expect(await pending()).toEqual({ frames: 0, observers: 0 });
    await page.evaluate(() => {
      (window as unknown as Record<string, unknown>).__blockHistory = true;
    });
    await expect(
      page.evaluate(runConversationChurn, {
        bob: "bob",
        channel: "channel",
        carol: "carol",
      }),
    ).rejects.toThrow("Conversation readiness exceeded 5000ms: bob, round 2");
    expect(await pending()).toEqual({ frames: 0, observers: 0 });
    await page.evaluate(() => {
      (window as unknown as Record<string, unknown>).__blockHistory = false;
    });
    expect(
      await page.evaluate(runConversationChurn, {
        bob: "bob",
        channel: "channel",
        carol: "carol",
      }),
    ).toEqual({ rounds: 24, activations: 72 });
    expect(await pending()).toEqual({ frames: 0, observers: 0 });
  });

  for (const phase of ["mutation", "sample"] as const) {
    test(`conversation readiness rejects expired ${phase} observations`, async ({
      page,
    }) => {
      await page.setContent(`
        <button data-testid="conversation-row-bob">Bob</button>
        <button data-testid="conversation-row-channel">Channel</button>
        <button data-testid="conversation-row-carol">Carol</button>
        <div data-testid="conversation-header">Conversation</div>
        <div role="log"><span></span></div>
        <div data-testid="conversation-empty">Empty</div>
      `);
      await page.evaluate((phase) => {
        const now = performance.now.bind(performance);
        const rect = Element.prototype.getBoundingClientRect;
        const NativeObserver = window.MutationObserver;
        let offset = 0;
        Object.defineProperty(performance, "now", {
          configurable: true,
          value: () => now() + offset,
        });
        if (phase === "mutation")
          window.MutationObserver = class extends NativeObserver {
            override observe(target: Node, options?: MutationObserverInit) {
              super.observe(target, options);
              queueMicrotask(() => {
                offset = 5001;
              });
            }
          };
        else
          Element.prototype.getBoundingClientRect = function () {
            const value = rect.call(this);
            if (this.getAttribute("role") === "log") offset = 5001;
            return value;
          };
        for (const button of document.querySelectorAll("button"))
          button.addEventListener("click", () => {
            const commit = () => {
              for (const row of document.querySelectorAll("button"))
                row.setAttribute("aria-current", String(row === button));
              const id = button.dataset.testid?.replace(
                "conversation-row-",
                "",
              );
              document.querySelector('[role="log"] span')!.textContent =
                id === "bob"
                  ? "history 299"
                  : id === "channel"
                    ? "channel kickoff"
                    : "";
            };
            if (phase === "mutation") queueMicrotask(commit);
            else commit();
          });
        (window as unknown as Record<string, unknown>).__restoreDeadlineClock =
          () => {
            delete (performance as unknown as Record<string, unknown>).now;
            Element.prototype.getBoundingClientRect = rect;
            window.MutationObserver = NativeObserver;
          };
      }, phase);
      try {
        await expect(
          page.evaluate(runConversationChurn, {
            bob: "bob",
            channel: "channel",
            carol: "carol",
          }),
        ).rejects.toThrow(
          "Conversation readiness exceeded 5000ms: bob, round 2",
        );
      } finally {
        await page.evaluate(() =>
          (
            (window as unknown as Record<string, unknown>)
              .__restoreDeadlineClock as () => void
          )(),
        );
      }
    });
  }

  test("rapid conversation switching does not accumulate DOM bubbles", async ({
    page,
  }) => {
    test.setTimeout(90_000);
    await enterChat(page);

    // Seed Bob's DM with a large history so a leak would be obvious.
    await page.evaluate(() => {
      const w = window as unknown as Record<string, unknown>;
      const inject = w.__mockInject as (
        c: string,
        t: string,
        who: string,
      ) => void;
      for (let i = 0; i < 300; i++) {
        inject("acc:acc_bob_bbbb2222", `history ${i}`, "device_bob_2222");
      }
    });

    // Keep one complete pointer round and final pointer navigation. The repeated
    // rounds measure list replacement, not trusted-input or pointer latency.
    const activate = async (id: string) => {
      const row = page.getByTestId(`conversation-row-${id}`);
      await expect(row).toBeVisible();
      await row.click();
      await expect(row).toHaveAttribute("aria-current", "true");
    };
    await activate(BOB.account);
    await expect(page.getByTestId("conversation-header")).toBeVisible();
    await expect(page.getByRole("log").getByText("history 299")).toBeVisible();
    await activate(CHANNEL.id);
    await expect(
      page.getByRole("log").getByText("channel kickoff"),
    ).toBeVisible();
    await activate(CAROL.account);
    await expect(page.getByTestId("conversation-empty")).toBeVisible();

    // One browser journey avoids per-action protocol round trips. Activate the
    // real buttons and await each committed, visible destination before moving on.
    const churn = await page.evaluate(runConversationChurn, {
      bob: BOB.account,
      channel: CHANNEL.id,
      carol: CAROL.account,
    });
    expect(churn).toEqual({ rounds: 24, activations: 72 });

    // End on Bob's large-history DM; the DOM still holds only a bounded window.
    await page.getByTestId(`conversation-row-${BOB.account}`).click();
    await expect(page.getByTestId("conversation-header")).toBeVisible();
    // Wait for the (now large) history to actually render before counting.
    await expect(page.getByTestId("message-bubble").first()).toBeVisible();
    const rendered = await bubbleCount(page);
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThan(MAX_RENDERED);
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });
});
