import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB, CHANNEL } from "./helpers/session";

test("incoming unread count reaches the OS badge and clears on read and logout", async ({
  page,
}) => {
  test.setTimeout(process.platform === "win32" ? 90_000 : 30_000);
  await enterChat(page);
  await page.evaluate(() => {
    const w = window as unknown as Window & {
      __badgeCalls: Array<{ count: number; owner: string; revision: number }>;
      __mockEmit: (event: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    w.__badgeCalls = [];
    const invoke = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "set_badge")
        w.__badgeCalls.push(
          args as { count: number; owner: string; revision: number },
        );
      return invoke(cmd, args);
    };
  });

  const incoming = async (text: string) => {
    await page.evaluate(
      ({ message, device }) => {
        const w = window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        };
        w.__mockEmit("dm-received", {
          from: device,
          from_name: "bob",
          text: message,
          reply_to: null,
        });
      },
      { message: text, device: BOB.device },
    );
  };
  const latestCount = () =>
    page.evaluate(() => {
      const w = window as unknown as {
        __badgeCalls: Array<{ count: number; owner: string; revision: number }>;
      };
      const { count, owner } = w.__badgeCalls[w.__badgeCalls.length - 1] ?? {};
      return { count, owner };
    });

  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await incoming("badge one");
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockEmit: (event: string, payload: unknown) => void;
    };
    w.__mockEmit("channel-message", {
      channel_id: "chan_team_dddd4444",
      channel_name: "team",
      from: "device_bob_2222",
      text: "another unread chat",
      reply_to: null,
    });
  });
  await expect.poll(latestCount).toEqual({ count: 2, owner: "u_self" });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(latestCount).toEqual({ count: 2, owner: "u_self" });
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", {
        exact: true,
      }),
  ).toBeVisible();
  await incoming("new while focused");
  await expect.poll(latestCount).toEqual({ count: 3, owner: "u_self" });
  await expect(
    page
      .getByTestId(`conversation-row-${CHANNEL.id}`)
      .locator("..")
      .getByText("1", {
        exact: true,
      }),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect.poll(latestCount).toEqual({ count: 2, owner: "u_self" });
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect.poll(latestCount).toEqual({ count: 0, owner: "u_self" });
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await incoming("badge two");
  await expect.poll(latestCount).toEqual({ count: 1, owner: "u_self" });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-sign-out").click();
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect.poll(latestCount).toEqual({ count: 0, owner: "u_self" });
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("other_user");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect.poll(latestCount).toEqual({ count: 0, owner: "u_other" });
  const revisions = await page.evaluate(() =>
    (
      window as unknown as { __badgeCalls: Array<{ revision: number }> }
    ).__badgeCalls.map((call) => call.revision),
  );
  expect(revisions).toEqual([...revisions].sort((a, b) => a - b));
  expect(new Set(revisions).size).toBe(revisions.length);
});
