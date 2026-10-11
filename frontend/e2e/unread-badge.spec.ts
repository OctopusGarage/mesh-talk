import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB, CHANNEL } from "./helpers/session";

async function observeBadge(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    const w = window as unknown as Window & {
      __badgeCounts: number[];
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    w.__badgeCounts = [];
    const invoke = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "set_badge") w.__badgeCounts.push(args.count as number);
      return invoke(cmd, args);
    };
  });
}

function latestBadgeCount(page: import("@playwright/test").Page) {
  return page.evaluate(() => {
    const counts = (window as unknown as { __badgeCounts: number[] })
      .__badgeCounts;
    return counts[counts.length - 1] ?? 0;
  });
}

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

test("an open chat remains unread while Mesh-Talk is in the background", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => false,
    });
    const w = window as unknown as Window & {
      __badgeCounts: number[];
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __mockEmit: (event: string, payload: unknown) => void;
    };
    w.__badgeCounts = [];
    const invoke = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "set_badge") w.__badgeCounts.push(args.count as number);
      return invoke(cmd, args);
    };
    w.__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "arrived while app was behind another window",
      reply_to: null,
    });
  });
  const count = () =>
    page.evaluate(() => {
      const counts = (window as unknown as { __badgeCounts: number[] })
        .__badgeCounts;
      return counts[counts.length - 1] ?? 0;
    });
  await expect.poll(count).toBe(1);
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => true,
    });
    window.dispatchEvent(new Event("focus"));
  });
  await expect.poll(count).toBe(0);
});

test("background channel messages and files clear only when that chat is viewed", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await observeBadge(page);
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => false,
    });
    const emit = (
      window as unknown as {
        __mockEmit: (event: string, value: unknown) => void;
      }
    ).__mockEmit;
    emit("channel-message", {
      channel_id: "chan_team_dddd4444",
      channel_name: "team",
      from: "device_bob_2222",
      text: "channel message in background",
      reply_to: null,
    });
    emit("file-received", {
      conv: "chan_team_dddd4444",
      is_channel: true,
      from: "device_bob_2222",
      name: "report.pdf",
      size: 2048,
      mime: "application/pdf",
      file_conv: "fc_unread_channel",
      media: false,
    });
    emit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "different chat",
      reply_to: null,
    });
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(3);
  await expect(
    page
      .getByTestId(`conversation-row-${CHANNEL.id}`)
      .locator("..")
      .getByText("2", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => true,
    });
    window.dispatchEvent(new Event("focus"));
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("foreground delivery is read only in the conversation being viewed", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await observeBadge(page);
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => true,
    });
    const emit = (
      window as unknown as {
        __mockEmit: (event: string, value: unknown) => void;
      }
    ).__mockEmit;
    emit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "visible message",
      reply_to: null,
    });
    emit("channel-message", {
      channel_id: "chan_team_dddd4444",
      channel_name: "team",
      from: "device_bob_2222",
      text: "unseen channel message",
      reply_to: null,
    });
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await expect(
    page
      .getByTestId(`conversation-row-${CHANNEL.id}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("unread messages survive reopening the same account and clear after reading", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "read after restart",
      reply_to: null,
    });
  });
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toBeVisible();
  await enterChat(page);
  await observeBadge(page);
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("early messages and files gain unread badges when their sender appears in discovery", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockEmit: (event: string, payload: unknown) => void;
      __mockAddPeer: (userId: string, accountId: string, name: string) => void;
    };
    w.__mockEmit("dm-received", {
      from: "device_new_9999",
      from_name: "New contact",
      text: "arrived before discovery",
      reply_to: null,
    });
    w.__mockEmit("file-received", {
      conv: "acc_new_9999",
      is_channel: false,
      from: "device_new_9999",
      name: "early.pdf",
      size: 512,
      mime: "application/pdf",
      file_conv: "fc_early_9999",
      media: false,
    });
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(2);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockAddPeer: (
          userId: string,
          accountId: string,
          name: string,
        ) => void;
      }
    ).__mockAddPeer("device_new_9999", "acc_new_9999", "New contact");
  });
  await expect(page.getByTestId("conversation-row-acc_new_9999")).toBeVisible();
  await expect.poll(() => latestBadgeCount(page)).toBe(2);
  await page.getByTestId("conversation-row-acc_new_9999").click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("history load failure leaves unread until the chat can be viewed", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockEmit: (event: string, payload: unknown) => void;
      __mockFailNext: (command: string, message?: string) => void;
    };
    w.__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "unread while history is unavailable",
      reply_to: null,
    });
    w.__mockFailNext("owner_account_history", "history unavailable");
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("an undiscovered sender remains unread across restart until discovery", async ({
  page,
}) => {
  // This scenario signs in twice; each mocked startup can take ~30s on Windows CI.
  test.setTimeout(process.platform === "win32" ? 120_000 : 30_000);
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("dm-received", {
      from: "device_late_9999",
      from_name: "Late contact",
      text: "before restart",
      reply_to: null,
    });
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        localStorage.getItem(
          "mesh-talk-pending-unread-v1:u_self:acc_self_aaaa1111",
        ),
      ),
    )
    .toContain("device_late_9999");
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockAddPeer: (
          userId: string,
          accountId: string,
          name: string,
        ) => void;
      }
    ).__mockAddPeer("device_late_9999", "acc_late_9999", "Late contact");
  });
  await expect(
    page.getByTestId("conversation-row-acc_late_9999"),
  ).toBeVisible();
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.getByTestId("conversation-row-acc_late_9999").click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("a replacement account does not inherit unread after identity lookup fails", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockEmit: (event: string, payload: unknown) => void;
      __mockFailNext: (command: string, message?: string) => void;
      __mockReplaceIdentity: (accountId: string) => void;
    };
    w.__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "old account unread",
      reply_to: null,
    });
    w.__mockFailNext("owner_node_identity", "node restarting");
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.evaluate(() =>
    (
      window as unknown as {
        __mockReplaceIdentity: (accountId: string) => void;
      }
    ).__mockReplaceIdentity("acc_replacement_9999"),
  );
  await expect.poll(() => latestBadgeCount(page), { timeout: 10_000 }).toBe(0);
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toHaveCount(0);
});
