import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, BOB } from "./helpers/session";

test("send-time countdown expires both directions without reopening and rejects stale history", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  const frozen = Date.now() + 1000;
  await page.clock.install({ time: new Date(frozen - 1000) });
  await page.clock.pauseAt(new Date(frozen));
  await page.getByTestId("composer-expiry").selectOption("60000");
  await page.getByTestId("composer-input").fill("temporary outgoing message");
  await page.getByTestId("composer-send").click();
  const outgoing = page
    .getByTestId("message-bubble")
    .filter({ hasText: "temporary outgoing message" });
  await expect(outgoing.getByTestId("message-expiry")).toHaveText("1:00");
  await page.evaluate(({ account, device }) => {
    const mock = window as unknown as {
      __mockInject: (
        conv: string,
        text: string,
        who: string,
        metadata: { wall_clock: number; expires_at_ms: number },
      ) => void;
      __mockEmit: (event: string, payload: unknown) => void;
    };
    mock.__mockInject(`acc:${account}`, "temporary incoming message", device, {
      wall_clock: Date.now(),
      expires_at_ms: Date.now() + 60_000,
    });
    mock.__mockEmit("dm-received", {
      from: device,
      from_name: "Bob",
      text: "temporary incoming message",
      reply_to: null,
    });
  }, BOB);
  const incoming = page
    .getByTestId("message-bubble")
    .filter({ hasText: "temporary incoming message" });
  await expect(incoming.getByTestId("message-expiry")).toHaveText("1:00");
  await page.clock.fastForward(1000);
  await expect(outgoing.getByTestId("message-expiry")).toHaveText("0:59");
  await page.clock.fastForward(60_000);
  await expect(outgoing).toHaveCount(0);
  await expect(incoming).toHaveCount(0);
  await page.getByTestId("conversation-history-trigger").click();
  await expect(page.getByTestId("conversation-history-dialog")).toBeVisible();
  await expect(
    page.getByText("temporary outgoing message", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("temporary incoming message", { exact: true }),
  ).toHaveCount(0);
});
