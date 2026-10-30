import { test, expect } from "./helpers/portable-evidence";
import { waitForChatShell, enterChat, BOB, CHANNEL } from "./helpers/session";

// Successful portable scenarios retain evidence in the full Chromium run.
test.use({ trace: "on" });

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
  await waitForChatShell(page);
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

test("clicking an already open conversation clears unread after app activation", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await observeBadge(page);
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => false,
    });
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "unread in the already selected conversation",
      reply_to: null,
    });
    window.dispatchEvent(new Event("focus"));
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await page.evaluate(() => {
    Object.defineProperty(document, "hasFocus", {
      configurable: true,
      value: () => true,
    });
  });
  await page
    .locator("main.conversation-canvas")
    .click({ position: { x: 30, y: 130 } });
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
  await expect(
    page
      .getByTestId(`conversation-row-${BOB.account}`)
      .locator("..")
      .getByText("1", { exact: true }),
  ).toHaveCount(0);
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
  await page
    .locator("main.conversation-canvas")
    .click({ position: { x: 30, y: 130 } });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
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

test("one logical DM remains one unread badge across duplicate live events and history scanning", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    w.__unreadHistoryRevision = 1;
    w.__dmHistoryScans = 0;
    internals.invoke = async (cmd, args) => {
      const result = await original(cmd, args);
      if (cmd === "owner_account_history" && w.__unreadHistoryRevision === 2)
        w.__dmHistoryScans = Number(w.__dmHistoryScans) + 1;
      if (cmd === "owner_node_identity")
        return {
          ...(result as object),
          history_revision: w.__unreadHistoryRevision,
        };
      return result;
    };
  });
  await expect
    .poll(
      () =>
        page.evaluate(() =>
          localStorage.getItem(
            "mesh-talk-reception-v2:u_self:acc_self_aaaa1111",
          ),
        ),
      { timeout: 10_000 },
    )
    .toContain(`account:${BOB.account}`);
  await page.evaluate(({ account, device }) => {
    const w = window as unknown as {
      __mockInject: (
        conv: string,
        text: string,
        who: string,
        metadata: { id: string },
      ) => void;
      __mockEmit: (event: string, payload: unknown) => void;
      __unreadHistoryRevision: number;
    };
    const id = "one-logical-dm";
    w.__mockInject(`acc:${account}`, "one new message", device, { id });
    const event = {
      from: device,
      from_account: account,
      message_id: id,
      from_name: "bob",
      text: "one new message",
      reply_to: null,
    };
    w.__mockEmit("dm-received", event);
    w.__mockEmit("dm-received", event);
    w.__unreadHistoryRevision = 2;
  }, BOB);
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  const row = page.getByTestId(`conversation-row-${BOB.account}`);
  await expect(row.locator("..").getByText("1", { exact: true })).toBeVisible();
  // The next roster/history scan reads the same stable logical ID.
  await expect
    .poll(
      () =>
        page.evaluate(() => {
          const saved = JSON.parse(
            localStorage.getItem(
              "mesh-talk-reception-v2:u_self:acc_self_aaaa1111",
            ) ?? "{}",
          );
          return JSON.stringify(saved);
        }),
      { timeout: 10_000 },
    )
    .toContain("one-logical-dm");
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (window as unknown as { __dmHistoryScans: number })
              .__dmHistoryScans,
        ),
      { timeout: 10_000 },
    )
    .toBeGreaterThan(0);
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await expect(row.locator("..").getByText("1", { exact: true })).toBeVisible();
  await row.click();
  await expect(
    page.getByTestId("message-bubble").filter({ hasText: "one new message" }),
  ).toHaveCount(1);
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});

test("channel mention indicator survives later traffic and clears when the mention is viewed", async ({
  page,
}) => {
  await enterChat(page);
  const row = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  await page.evaluate(
    ({ channel, device }) => {
      const w = window as unknown as {
        __mockInject: (
          conv: string,
          text: string,
          who: string,
          metadata: { id: string },
        ) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      for (const [id, text] of [
        ["sidebar-mention", "@tester please check"],
        ["sidebar-ordinary", "A later ordinary message"],
      ]) {
        w.__mockInject(`ch:${channel}`, text, "bob", { id });
        w.__mockEmit("channel-message", {
          channel_id: channel,
          channel_name: "team",
          from: device,
          text,
          event_id: id,
          reply_to: null,
        });
      }
    },
    { channel: CHANNEL.id, device: BOB.device },
  );
  await expect(row.getByText("[Mentioned you]", { exact: true })).toBeVisible();
  await expect(row.locator("..").getByText("2", { exact: true })).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(row.getByText("[Mentioned you]", { exact: true })).toBeVisible();
  await row.click();
  await expect(
    page.getByText("@tester please check", { exact: true }),
  ).toBeVisible();
  await expect(row).not.toContainText("[Mentioned you]");
});

test("unread review clears undiscovered senders and hidden channels without hiding later arrivals", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  const row = page.getByTestId(`conversation-row-${CHANNEL.id}`);
  await row.hover();
  await page.getByTestId(`conversation-actions-${CHANNEL.id}`).click();
  await page.getByRole("button", { name: "Hide channel", exact: true }).click();
  await page.evaluate(
    ({ channel, device }) => {
      const w = window as unknown as {
        __mockEmit: (event: string, data: unknown) => void;
      };
      w.__mockEmit("channel-message", {
        channel_id: channel,
        channel_name: "team",
        from: device,
        text: "hidden unread",
        event_id: "hidden-unread",
        reply_to: null,
      });
      w.__mockEmit("dm-received", {
        from: "unknown-source",
        from_name: "Unknown",
        text: "no conversation yet",
        reply_to: null,
      });
    },
    { channel: CHANNEL.id, device: BOB.device },
  );
  await expect.poll(() => latestBadgeCount(page)).toBe(2);
  await page.getByTestId("unread-review-trigger").click();
  const panel = page.getByRole("dialog", {
    name: "Unread messages",
    exact: true,
  });
  await expect(
    panel.getByText("Hidden conversation", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByText("Waiting to identify sender", { exact: true }),
  ).toBeVisible();
  await panel.getByTestId("clear-unread-pending:unknown-source").click();
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await panel
    .getByRole("button", { name: "Mark all as read", exact: true })
    .click();
  await expect(panel.getByText("You're all caught up.")).toBeVisible();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, data: unknown) => void;
      }
    ).__mockEmit("dm-received", {
      from: "another-unknown",
      from_name: "Another",
      text: "new arrival",
      reply_to: null,
    });
  });
  await expect.poll(() => latestBadgeCount(page)).toBe(1);
  await expect(
    panel.getByText("Waiting to identify sender", { exact: true }),
  ).toBeVisible();
  await panel
    .getByRole("button", { name: "Mark all as read", exact: true })
    .click();
  await page.keyboard.press("Escape");
  await enterChat(page);
  await page.getByTestId("unread-review-trigger").click();
  await expect(page.getByText("You're all caught up.")).toBeVisible();
});

test("unread review exposes failed badge updates and retries the current cleared count", async ({
  page,
}) => {
  await enterChat(page);
  await observeBadge(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockEmit: (event: string, data: unknown) => void;
      __mockFailNext: (command: string, message: string) => void;
    };
    w.__mockFailNext("set_badge", "badge temporarily unavailable");
    w.__mockEmit("dm-received", {
      from: "unknown-badge",
      from_name: "Unknown",
      text: "retry badge",
      reply_to: null,
    });
  });
  await page.getByTestId("unread-review-trigger").click();
  const panel = page.getByRole("dialog", {
    name: "Unread messages",
    exact: true,
  });
  await expect(panel.getByTestId("unread-badge-error")).toBeVisible();
  await panel
    .getByRole("button", { name: "Retry badge update", exact: true })
    .click();
  await expect(panel.getByTestId("unread-badge-error")).toHaveCount(0);
  await panel
    .getByRole("button", { name: "Mark all as read", exact: true })
    .click();
  await expect.poll(() => latestBadgeCount(page)).toBe(0);
});
