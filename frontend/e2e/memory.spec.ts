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
 * no real backend, no timers.
 */

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

async function enterChat(page: Page, user = "mem") {
  await register(page, user);
  await signIn(page, user);
  await expect(page.getByTestId("chat-shell")).toBeVisible();
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
    await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();

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
    await expect(page.getByText(`synthetic message ${N - 1}`)).toBeVisible();
    // ...but the DOM holds only the on-screen window, NOT all N bubbles.
    const rendered = await bubbleCount(page);
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThan(MAX_RENDERED);
    expect(rendered).toBeLessThan(N);
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("rapid conversation switching does not accumulate DOM bubbles", async ({
    page,
  }) => {
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

    // Churn between conversations many times. Each switch resets the keyed,
    // virtualized list; bubbles from the previous conversation must not linger.
    for (let i = 0; i < 25; i++) {
      await page.getByTestId(`conversation-row-${BOB.account}`).click();
      await expect(page.getByTestId("conversation-header")).toBeVisible();
      await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
      await expect(page.getByText("channel kickoff")).toBeVisible();
      await page.getByTestId(`conversation-row-${CAROL.account}`).click();
      await expect(page.getByTestId("conversation-empty")).toBeVisible();
    }

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
