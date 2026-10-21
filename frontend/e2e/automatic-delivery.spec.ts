import { test, expect } from "./helpers/portable-evidence";
import {
  enterChat,
  openBobDm,
  revealComposerTools,
  BOB,
} from "./helpers/session";
import { seedMarketPacks } from "./helpers/packs";

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
