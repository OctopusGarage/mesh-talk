import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, BOB } from "./helpers/session";

test("text, sticker and file card stay Awaiting until explicit authoritative mock projection", async ({
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
  await page.getByTestId("composer-stickers").click();
  await page.getByTestId("sticker-option-1f602").click();
  await expect(page.locator('[data-delivery="awaiting"]')).toHaveCount(2);
  await page.getByTestId("composer-attach").click();
  await expect(page.getByText("card.txt")).toBeVisible();
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
