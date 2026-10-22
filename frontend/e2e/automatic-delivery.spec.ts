import { test, expect } from "./helpers/portable-evidence";
import {
  enterChat,
  openBobDm,
  revealComposerTools,
  BOB,
} from "./helpers/session";
import { seedMarketPacks } from "./helpers/packs";
import { seedThemeBeforeLoad } from "./helpers/session";
import type { Page } from "@playwright/test";

async function observeLocalDelivery(page: Page) {
  await page.evaluate(() => {
    const scope = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __deliveryDetailsFixture: {
        observation: { kind: string; age_ms: number; reason?: string } | null;
        hold: boolean;
        entered: boolean;
        seenKind: string | null;
        release: () => void;
      };
    };
    const state = {
      observation: {
        kind: "retry_scheduled",
        age_ms: 180000,
        reason: "post_office_disabled",
      } as {
        kind: string;
        age_ms: number;
        reason?: string;
      } | null,
      hold: false,
      entered: false,
      seenKind: null as string | null,
      release: () => {},
    };
    scope.__deliveryDetailsFixture = state;
    const original = scope.__TAURI_INTERNALS__.invoke;
    scope.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const result = await original(command, args);
      if (command !== "owner_delivery_statuses") return result;
      state.seenKind = state.observation?.kind ?? null;
      const rows = (result as Array<{ id: string; status: string }>).map(
        (row) => ({
          ...row,
          ...(row.status === "awaiting" && state.observation
            ? { observation: state.observation }
            : {}),
        }),
      );
      if (!state.hold) return rows;
      state.entered = true;
      return new Promise((resolve) => {
        state.release = () => resolve(rows);
      });
    };
  });
}

for (const viewport of [
  { width: 1280, height: 800, theme: "dark" },
  { width: 760, height: 620, theme: "light" },
  { width: 320, height: 360, theme: "dark" },
]) {
  test(`delivery details preserve keyboard focus and fit ${viewport.width}x${viewport.height}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await seedThemeBeforeLoad(page, viewport.theme);
    await enterChat(page);
    await openBobDm(page);
    await observeLocalDelivery(page);
    await page
      .getByTestId("composer-input")
      .fill("Inspect this local delivery");
    await page.getByTestId("composer-send").click();
    const bubble = page
      .getByTestId("message-bubble")
      .filter({ hasText: "Inspect this local delivery" });
    const trigger = bubble.getByTestId("delivery-details-trigger");
    await expect(trigger).toBeVisible();
    await trigger.focus();
    await trigger.press("Enter");
    const details = page.getByTestId("delivery-details");
    await expect(details).toBeVisible();
    await expect(details).toHaveAccessibleName("Delivery details");
    await expect(details).toContainText(/Retry was scheduled .*minutes ago\./);
    await expect(details).toContainText(
      "Post-office sending was disabled on this device",
    );
    await expect(
      details.getByRole("button", { name: "Review delivery settings" }),
    ).toBeVisible();
    await expect(details.locator("[aria-live]")).toHaveCount(0);
    await expect(bubble.getByRole("button", { name: /resend/i })).toHaveCount(
      0,
    );
    const bounds = await details.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.y).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(viewport.width);
    expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(viewport.height);
    expect(
      await details.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: testInfo.outputPath("delivery-details-open.png"),
    });
    await details.press("Escape");
    await expect(details).toBeHidden();
    await expect(trigger).toBeFocused();
    await trigger.press("Space");
    await expect(details).toBeVisible();
    await details
      .getByRole("button", { name: "Review delivery settings" })
      .click();
    const settings = page
      .getByRole("dialog")
      .filter({ has: page.locator("#setting-post-office") });
    await expect(settings).toBeVisible();
    await expect(page.locator("#setting-post-office")).toBeFocused();
    await settings.press("Escape");
    await expect(settings).toBeHidden();
    await expect(trigger).toBeFocused();
    await page.evaluate(() => {
      (
        window as unknown as {
          __deliveryDetailsFixture: { observation: unknown };
        }
      ).__deliveryDetailsFixture.observation = {
        kind: "retry_scheduled",
        age_ms: 0,
        reason: "no_known_route",
      };
    });
    await trigger.focus();
    await trigger.press("Enter");
    await expect(details).toContainText(
      "No eligible route was known to this device",
    );
    await details
      .getByRole("button", { name: "Open connection diagnostics" })
      .click();
    const diagnostics = page.getByRole("dialog");
    await expect(diagnostics).toBeVisible();
    await diagnostics.press("Escape");
    await expect(diagnostics).toBeHidden();
    await expect(trigger).toBeFocused();
    await trigger.focus();
    await trigger.press("Enter");
    await page.evaluate(() => {
      (
        window as unknown as {
          __deliveryDetailsFixture: { observation: unknown };
        }
      ).__deliveryDetailsFixture.observation = null;
    });
    await expect(details).toContainText(
      "No current local attempt details are available.",
    );
    await page.evaluate(() => {
      (
        window as unknown as {
          __deliveryDetailsFixture: { observation: unknown };
        }
      ).__deliveryDetailsFixture.observation = {
        kind: "future_phase",
        age_ms: 0,
      };
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __deliveryDetailsFixture: { seenKind: string | null };
              }
            ).__deliveryDetailsFixture.seenKind,
        ),
      )
      .toBe("future_phase");
    await expect(details).toContainText(
      "No current local attempt details are available.",
    );
    await page.evaluate((account) => {
      const scope = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<Array<{ id: string }>>;
        };
        __mockSetDelivery: (
          account: string,
          id: string,
          status: string,
        ) => void;
      };
      void scope.__TAURI_INTERNALS__
        .invoke("owner_account_history", {
          owner: "u_self",
          account,
          limit: 500,
        })
        .then((rows) => {
          scope.__mockSetDelivery(
            account,
            rows[rows.length - 1]!.id,
            "delivered",
          );
        });
    }, BOB.account);
    await expect(details).toBeHidden();
    await expect(trigger).toHaveCount(0);
    await expect(bubble.locator('[data-delivery="delivered"]')).toBeVisible();
  });
}

test("delivery details close on navigation and app lock, including a late status response", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await observeLocalDelivery(page);
  await page.getByTestId("composer-input").fill("Owner-scoped delivery detail");
  await page.getByTestId("composer-send").click();
  const trigger = page.getByTestId("delivery-details-trigger");
  await trigger.click();
  await expect(page.getByTestId("delivery-details")).toBeVisible();
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  await expect(page.getByTestId("delivery-details")).toBeHidden();
  await openBobDm(page);
  await expect(page.getByTestId("delivery-details")).toBeHidden();
  await trigger.click();
  await expect(page.getByTestId("delivery-details")).toBeVisible();
  await page.evaluate(() => {
    (
      window as unknown as { __deliveryDetailsFixture: { hold: boolean } }
    ).__deliveryDetailsFixture.hold = true;
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __deliveryDetailsFixture: { entered: boolean };
            }
          ).__deliveryDetailsFixture.entered,
      ),
    )
    .toBe(true);
  await page.evaluate(async () => {
    const scope = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __mockEmit: (event: string, payload: unknown) => void;
      __deliveryDetailsFixture: { release: () => void };
    };
    // macOS automatically attempts biometric unlock on the lock screen.
    // Keep this privacy scenario locked by modelling cancelled authentication.
    const original = scope.__TAURI_INTERNALS__.invoke;
    scope.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "unlock_app") throw new Error("Authentication cancelled");
      return original(command, args);
    };
    await scope.__TAURI_INTERNALS__.invoke("lock_app", {});
    scope.__mockEmit("app-locked", null);
    scope.__deliveryDetailsFixture.release();
  });
  await expect(page.getByTestId("chat-shell")).toBeHidden();
  await expect(page.getByTestId("delivery-details")).toBeHidden();
  await expect(page.getByTestId("delivery-details-trigger")).toHaveCount(0);
});

test("text, sticker and file card stay Awaiting until explicit authoritative mock projection", async ({
  page,
}) => {
  await seedMarketPacks(page, ["cats"]);
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    const accepted: string[] = [];
    w.__acceptedDeliveryIds = accepted;
    internals.invoke = async (cmd, args) => {
      if (cmd === "plugin:dialog|open") return "/tmp/card.txt";
      const result = await original(cmd, args);
      if (cmd === "owner_enqueue_text" || cmd === "owner_enqueue_sticker")
        accepted.push(result as string);
      if (cmd === "owner_enqueue_file")
        accepted.push((result as { id: string }).id);
      return result;
    };
  });
  await page.getByTestId("composer-input").fill("automatic delivery");
  await page.getByTestId("composer-send").click();
  await expect(page.locator('[data-delivery="awaiting"]')).toHaveCount(1);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByTestId("sticker-pack-cats").click();
  await page.getByTestId("sticker-option-pack:cats:cat-01-01").click();
  await expect(page.locator('[data-delivery="awaiting"]')).toHaveCount(2);
  await revealComposerTools(page);
  await page.getByTestId("composer-attach").click();
  await expect(
    page.getByTestId("message-bubble").filter({ hasText: "card.txt" }),
  ).toBeVisible();
  await expect(page.locator('[data-delivery="awaiting"]')).toHaveCount(3);
  await expect(page.locator('[data-delivery="delivered"]')).toHaveCount(0);
  const awaitingCard = page
    .getByTestId("message-bubble")
    .filter({ hasText: "card.txt" });
  await awaitingCard.getByTestId("delivery-details-trigger").click();
  const fileDetails = page.getByTestId("delivery-details");
  await expect(fileDetails).toContainText(
    "File download and reading are separate.",
  );
  await expect(fileDetails).not.toContainText("File card delivered.");
  await fileDetails.press("Escape");
  // This is a UI presentation mock, not authenticated receipt protocol evidence.
  await page.evaluate((account) => {
    const w = window as unknown as {
      __acceptedDeliveryIds: string[];
      __mockSetDelivery: (account: string, id: string, status: string) => void;
    };
    for (const id of w.__acceptedDeliveryIds)
      w.__mockSetDelivery(account, id, "delivered");
  }, BOB.account);
  await expect(page.locator('[data-delivery="delivered"]')).toHaveCount(3);
  const cardReceipt = page
    .getByTestId("message-bubble")
    .filter({ hasText: "card.txt" })
    .locator('[data-delivery="delivered"]');
  await expect(cardReceipt).toHaveAccessibleName("File offer received");
  await expect(
    cardReceipt.getByText("File offer received", { exact: true }),
  ).toBeVisible();
  await cardReceipt.focus();
  await expect(cardReceipt.getByRole("tooltip")).toBeVisible();
  await expect(cardReceipt.getByRole("tooltip")).toContainText(
    "File card delivered",
  );
});

test("a native picker completed after conversation navigation cannot send to the new conversation", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    const state = {
      enqueues: 0,
      entered: false,
      release: (_path: string) => {},
    };
    w.__pickerRace = state;
    internals.invoke = async (cmd, args) => {
      if (cmd === "plugin:dialog|open") {
        state.entered = true;
        return new Promise((resolve) => {
          state.release = resolve;
        });
      }
      if (cmd === "owner_enqueue_file") state.enqueues++;
      return original(cmd, args);
    };
  });
  await revealComposerTools(page);
  await page.getByTestId("composer-attach").click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerRace: { entered: boolean } })
            .__pickerRace.entered,
      ),
    )
    .toBe(true);
  await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
  const enqueues = await page.evaluate(async () => {
    const state = (
      window as unknown as {
        __pickerRace: { enqueues: number; release: (path: string) => void };
      }
    ).__pickerRace;
    state.release("/tmp/stale.txt");
    await new Promise((resolve) => setTimeout(resolve, 0));
    return state.enqueues;
  });
  expect(enqueues).toBe(0);
  await expect(page.getByText("stale.txt")).toHaveCount(0);
});

test("a pending local text acceptance moves the submitted text out of the composer", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    internals.invoke = async (cmd, args) => {
      if (cmd === "owner_enqueue_text") return new Promise(() => {});
      return original(cmd, args);
    };
  });
  const input = page.getByTestId("composer-input");
  await input.fill("queued during hotspot handoff");
  await page.getByTestId("composer-send").click();
  await expect(
    page
      .getByTestId("message-bubble")
      .filter({ hasText: "queued during hotspot handoff" }),
  ).toHaveCount(1);
  await expect(input).toHaveValue("");
  await input.fill("my next draft");
  await expect(input).toHaveValue("my next draft");
  await expect(page.getByTestId("composer-send")).toBeEnabled();
});

for (const accepted of [true, false]) {
  test(`submitted text stays in its bubble after ${accepted ? "acceptance" : "rejection"} and navigation`, async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    await page.evaluate(() => {
      const w = window as unknown as Record<string, unknown>;
      const internals = w.__TAURI_INTERNALS__ as {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      const original = internals.invoke;
      internals.invoke = async (cmd, args) => {
        if (cmd === "owner_enqueue_text")
          return new Promise((resolve, reject) => {
            w.__settleHandoffSend = (accepted: boolean) => {
              if (accepted) resolve(original(cmd, args));
              else reject(new Error("local acceptance rejected"));
            };
          });
        return original(cmd, args);
      };
    });
    const submitted = "submitted exactly once during handoff";
    const input = page.getByTestId("composer-input");
    await input.fill(submitted);
    await page.getByTestId("composer-send").click();
    await expect(input).toHaveValue("");
    const bubble = page
      .getByTestId("message-bubble")
      .filter({ hasText: submitted });
    await expect(bubble).toHaveCount(1);
    await input.fill("a separate next draft");
    await page.getByTestId("conversation-row-acc_carol_cccc3333").click();
    await openBobDm(page);
    await expect(input).toHaveValue("a separate next draft");
    await page.evaluate((accepted) => {
      (
        window as unknown as {
          __settleHandoffSend: (accepted: boolean) => void;
        }
      ).__settleHandoffSend(accepted);
    }, accepted);
    if (accepted)
      await expect(bubble.locator('[data-delivery="awaiting"]')).toBeVisible();
    else
      await expect(bubble.getByRole("button", { name: "Retry" })).toBeVisible();
    await expect(input).toHaveValue("a separate next draft");
    await expect(bubble).toHaveCount(1);
  });
}

test("a scoped native delivery invalidation refreshes status without a polling response", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args: Record<string, unknown>,
        ): Promise<unknown>;
      };
      __mockEmit(event: string, payload: unknown): void;
      __mockSetDelivery(account: string, id: string, status: string): void;
      __pushDelivery: { id: string; allowed: boolean; reads: number };
    };
    const original = host.__TAURI_INTERNALS__.invoke.bind(
      host.__TAURI_INTERNALS__,
    );
    host.__pushDelivery = { id: "", allowed: false, reads: 0 };
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "owner_delivery_statuses") {
        if (!host.__pushDelivery.allowed)
          return (args.ids as string[])
            .filter((id) => id === host.__pushDelivery.id)
            .map((id) => ({ id, status: "awaiting" }));
        host.__pushDelivery.reads++;
      }
      const result = await original(command, args);
      if (command === "owner_enqueue_text")
        host.__pushDelivery.id = result as string;
      return result;
    };
  });
  await page.getByTestId("composer-input").fill("push driven status");
  await page.getByTestId("composer-send").click();
  await expect(page.locator('[data-delivery="awaiting"]')).toHaveCount(1);
  await page.evaluate(async (account) => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args: Record<string, unknown>,
        ): Promise<unknown>;
      };
      __mockEmit(event: string, payload: unknown): void;
      __mockSetDelivery(account: string, id: string, status: string): void;
      __pushDelivery: { id: string; allowed: boolean; reads: number };
    };
    const identity = (await host.__TAURI_INTERNALS__.invoke(
      "owner_node_identity",
      { owner: "u_self" },
    )) as Record<string, string>;
    host.__mockSetDelivery(account, host.__pushDelivery.id, "delivered");
    host.__pushDelivery.allowed = true;
    host.__mockEmit("node-state-changed", { ...identity, kind: "delivery" });
    host.__pushDelivery.allowed = false;
  }, BOB.account);
  await expect(page.locator('[data-delivery="delivered"]')).toHaveCount(1);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __pushDelivery: { reads: number } })
          .__pushDelivery.reads,
    ),
  ).toBe(1);
});

test("delivery recovery survives its receipt and returns focus safely, then retires on logout", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await observeLocalDelivery(page);
  await page
    .getByTestId("composer-input")
    .fill("Keep recovery open through receipt");
  await page.getByTestId("composer-send").click();
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "Keep recovery open through receipt" });
  const trigger = bubble.getByTestId("delivery-details-trigger");
  await trigger.focus();
  await trigger.press("Enter");
  await page
    .getByTestId("delivery-details")
    .getByRole("button", { name: "Review delivery settings" })
    .click();
  const settings = page.getByTestId("settings-dialog");
  await expect(settings).toBeVisible();
  await page.evaluate(async (account) => {
    const scope = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args: Record<string, unknown>,
        ): Promise<{ id: string }[]>;
      };
      __mockSetDelivery(account: string, id: string, status: string): void;
    };
    const rows = await scope.__TAURI_INTERNALS__.invoke(
      "owner_account_history",
      { owner: "u_self", account, limit: 500 },
    );
    scope.__mockSetDelivery(account, rows[rows.length - 1].id, "delivered");
  }, BOB.account);
  await expect(trigger).toHaveCount(0);
  await expect(settings).toBeVisible();
  await settings.press("Escape");
  await expect(settings).toBeHidden();
  await expect(page.getByTestId("chat-shell")).toBeFocused();
  await page.getByTestId("composer-input").fill("Retire recovery on logout");
  await page.getByTestId("composer-send").click();
  const next = page
    .getByTestId("message-bubble")
    .filter({ hasText: "Retire recovery on logout" })
    .getByTestId("delivery-details-trigger");
  await next.focus();
  await next.press("Enter");
  await page
    .getByTestId("delivery-details")
    .getByRole("button", { name: "Review delivery settings" })
    .click();
  await expect(settings).toBeVisible();
  await page.evaluate(async () => {
    const modulePath = "/src/store/auth.ts";
    const { useAuth } = await import(/* @vite-ignore */ modulePath);
    await useAuth.getState().logout();
  });
  await expect(settings).toBeHidden();
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
});

test("channel acceptance reconciles repeated text once and confirmed post-office custody clears the waiting indicator", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __channelCustody: boolean;
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__channelCustody = false;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "send_channel_message")
        throw new Error("Legacy fanout IPC must not gate local acceptance");
      if (cmd === "channel_custody_statuses")
        return (args.ids as string[]).map((id) => ({
          id,
          replicas: 0,
          post_offices: w.__channelCustody ? 1 : 0,
        }));
      return original(cmd, args);
    };
  });
  for (let i = 0; i < 2; i++) {
    await page.getByTestId("composer-input").fill("same channel text");
    await page.getByTestId("composer-send").click();
  }
  const bubbles = page
    .getByTestId("message-bubble")
    .filter({ hasText: "same channel text" });
  await expect(bubbles).toHaveCount(2);
  await expect(bubbles.filter({ hasText: "sending…" })).toHaveCount(0);
  await expect(bubbles.locator('[data-custody="0"]')).toHaveCount(2);
  await page.evaluate(() => {
    (window as unknown as { __channelCustody: boolean }).__channelCustody =
      true;
  });
  await expect(bubbles.locator('[data-custody="1"]')).toHaveCount(2);
  await expect(
    bubbles.getByRole("img", { name: "Post office confirmed a stored copy" }),
  ).toHaveCount(2);
  await expect(bubbles).toHaveCount(2);
});
