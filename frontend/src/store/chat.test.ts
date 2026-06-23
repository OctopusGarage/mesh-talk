import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

// Capture the event handlers start() registers, so we can drive inbound events.
const { captured } = vi.hoisted(() => ({
  captured: { current: null as null | Record<string, (e: unknown) => void> },
}));
vi.mock("@/lib/events", () => ({
  subscribeNodeEvents: (h: Record<string, (e: unknown) => void>) => {
    captured.current = h;
    return () => {};
  },
}));

import { useChat, convKey } from "./chat";

const reset = () =>
  useChat.setState({
    ready: false,
    myId: "me",
    myAccountId: "myacct",
    peers: [],
    accounts: [],
    channels: [],
    active: null,
    messages: {},
    reactions: {},
    unread: {},
    members: [],
    incomingFiles: [],
    cacheOrder: [],
  });

beforeEach(() => {
  invoke.mockReset();
  reset();
});

describe("convKey", () => {
  it("namespaces by kind + id", () => {
    expect(convKey({ kind: "account", id: "a1", name: "x" })).toBe(
      "account:a1",
    );
    expect(convKey({ kind: "channel", id: "c1", name: "y" })).toBe(
      "channel:c1",
    );
  });
});

describe("open", () => {
  it("loads history + reactions, clears unread, sets active", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "account_history")
        return Promise.resolve([
          {
            id: "e1",
            from_me: false,
            who: "alice",
            text: "hello",
            wall_clock: 1000,
            reply_to: null,
          },
        ]);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ unread: { "account:a1": 3 } });
    await useChat.getState().open({ kind: "account", id: "a1", name: "A" });

    const s = useChat.getState();
    expect(s.active).toEqual({ kind: "account", id: "a1", name: "A" });
    expect(s.messages["account:a1"]).toHaveLength(1);
    expect(s.messages["account:a1"][0].text).toBe("hello");
    expect(s.unread["account:a1"]).toBe(0);
  });
});

describe("conversation cache LRU", () => {
  it("evicts the least-recently-opened caches beyond the cap, never the active one", async () => {
    // Every open() resolves empty history/reactions; the cache entry is the (empty) array.
    invoke.mockResolvedValue([]);
    // Open 60 distinct conversations (cap is 50).
    for (let i = 0; i < 60; i++) {
      await useChat
        .getState()
        .open({ kind: "account", id: `a${i}`, name: "x" });
    }
    const s = useChat.getState();
    // At most 50 cached message arrays remain.
    expect(Object.keys(s.messages).length).toBeLessThanOrEqual(50);
    expect(Object.keys(s.reactions).length).toBeLessThanOrEqual(50);
    expect(s.cacheOrder.length).toBeLessThanOrEqual(50);
    // The most-recently-opened (active) is retained; the oldest are evicted.
    expect(s.messages["account:a59"]).toBeDefined();
    expect(s.messages["account:a0"]).toBeUndefined();
    expect(s.messages["account:a9"]).toBeUndefined();
    // The active conversation key matches the last opened.
    expect(convKey(s.active!)).toBe("account:a59");
  });

  it("repopulates an evicted conversation when reopened", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "account_history")
        return Promise.resolve([
          {
            id: "e1",
            from_me: false,
            who: "alice",
            text: "back",
            wall_clock: 1,
            reply_to: null,
          },
        ]);
      return Promise.resolve([]);
    });
    for (let i = 0; i < 60; i++) {
      await useChat
        .getState()
        .open({ kind: "account", id: `a${i}`, name: "x" });
    }
    expect(useChat.getState().messages["account:a0"]).toBeUndefined();
    await useChat.getState().open({ kind: "account", id: "a0", name: "x" });
    expect(useChat.getState().messages["account:a0"]).toHaveLength(1);
    expect(useChat.getState().messages["account:a0"][0].text).toBe("back");
  });
});

describe("send", () => {
  it("sends then reloads so the message gets its real id", async () => {
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_to_account") {
        const a = args as { text: string };
        history = [
          {
            id: "e1",
            from_me: true,
            who: "me",
            text: a.text,
            wall_clock: 1,
            reply_to: null,
          },
        ];
        return Promise.resolve();
      }
      if (cmd === "account_history") return Promise.resolve(history);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("hi", null);

    expect(invoke).toHaveBeenCalledWith("send_to_account", {
      account: "a1",
      text: "hi",
      replyTo: null,
    });
    const msgs = useChat.getState().messages["account:a1"];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "e1", text: "hi", fromMe: true });
    expect(msgs[0].pending).toBeFalsy();
  });

  it("ignores an empty message", async () => {
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("   ", null);
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("toggleReaction", () => {
  // For an ACCOUNT conversation, reaction `who` is keyed by ACCOUNT id (myAccountId),
  // NOT the device id — the self-check must use myAccountId or the reaction can never be
  // toggled off (regression: account reactions were compared against the device myId).
  beforeEach(() => {
    invoke.mockResolvedValue([]);
    useChat.setState({
      active: { kind: "account", id: "a1", name: "A" },
      reactions: {
        "account:a1": [{ target: "t1", emoji: "👍", who: ["myacct"] }],
      },
    });
  });
  it("removes my own account reaction (who keyed by account id)", async () => {
    await useChat.getState().toggleReaction("t1", "👍");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "👍",
      remove: true,
    });
  });
  it("adds a reaction I have not made", async () => {
    await useChat.getState().toggleReaction("t1", "🎉");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "🎉",
      remove: false,
    });
  });
  it("matches the DEVICE id for channel conversations", async () => {
    useChat.setState({
      active: { kind: "channel", id: "c1", name: "C" },
      reactions: { "channel:c1": [{ target: "t1", emoji: "👍", who: ["me"] }] },
    });
    await useChat.getState().toggleReaction("t1", "👍");
    expect(invoke).toHaveBeenCalledWith("react_channel", {
      channelId: "c1",
      target: "t1",
      emoji: "👍",
      remove: true,
    });
  });
});

describe("incoming events", () => {
  const peer = {
    user_id: "dev1",
    account_id: "acctA",
    name: "Alice",
    addr: "1.2.3.4:7000",
    post_office: false,
  };

  async function boot() {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "my_id":
          return Promise.resolve("me");
        case "account_id":
          return Promise.resolve("myacct");
        case "list_peers":
          return Promise.resolve([peer]);
        default:
          return Promise.resolve([]);
      }
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    return stop;
  }

  it("re-publishes our persisted own avatar on boot (so peers pull it after restart)", async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "my_id":
          return Promise.resolve("me");
        case "account_id":
          return Promise.resolve("myacct");
        case "get_avatars":
          // a previously-set own avatar, persisted across the restart
          return Promise.resolve({ myacct: "data:image/png;base64,AAAA" });
        default:
          return Promise.resolve([]);
      }
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("publish_avatar", {
        avatar: "data:image/png;base64,AAAA",
      }),
    );
    stop();
  });

  it("routes a DM to the sender's ACCOUNT conversation and bumps unread", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "dev1",
      from_name: "Alice",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    stop();
  });

  it("does not bump unread for a DM from an undiscovered peer", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "ghost",
      from_name: "?",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:ghost"]).toBeUndefined();
    stop();
  });

  it("routes a channel message to its channel conversation", async () => {
    const stop = await boot();
    captured.current!.onChannelMessage({
      channel_id: "c1",
      channel_name: "general",
      from: "dev1",
      text: "yo",
      reply_to: null,
    });
    expect(useChat.getState().unread["channel:c1"]).toBe(1);
    stop();
  });

  it("de-dupes received files by file_conv", async () => {
    const stop = await boot();
    const f = {
      conv: "x",
      from: "dev1",
      name: "a.pdf",
      size: 10,
      mime: "application/pdf",
      file_conv: "fc1",
    };
    captured.current!.onFile(f);
    captured.current!.onFile(f);
    expect(useChat.getState().incomingFiles).toHaveLength(1);
    expect(useChat.getState().incomingFiles[0].fromName).toBe("Alice");
    stop();
  });
});

describe("send/action failures", () => {
  const acct = { kind: "account" as const, id: "a1", name: "A" };

  it("keeps the optimistic bubble (marked failed) when the send rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "send_to_account")
        return Promise.reject({ kind: "service", message: "node down" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);

    const msgs = useChat.getState().messages[convKey(acct)] ?? [];
    expect(msgs).toHaveLength(1);
    expect(msgs[0].text).toBe("hi");
    expect(msgs[0].failed).toBe(true);
    expect(msgs[0].pending).toBe(false);
    // "node down" maps to the relay-unreachable coarse reason.
    expect(msgs[0].failReason).toBe("relay-unreachable");
  });

  it("retry re-sends a failed bubble and clears the failed state on success", async () => {
    let history: unknown[] = [];
    let fail = true;
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_to_account") {
        if (fail)
          return Promise.reject({ kind: "service", message: "no peer" });
        const a = args as { text: string };
        history = [
          {
            id: "e1",
            from_me: true,
            who: "me",
            text: a.text,
            wall_clock: 1,
            reply_to: null,
          },
        ];
        return Promise.resolve();
      }
      if (cmd === "account_history") return Promise.resolve(history);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);

    const failed = useChat.getState().messages[convKey(acct)][0];
    expect(failed.failed).toBe(true);
    expect(failed.failReason).toBe("peer-unknown");
    const clientId = failed.clientId!;
    expect(clientId).toBeTruthy();

    // The send now succeeds; retry should re-dispatch and reconcile from the log.
    fail = false;
    await useChat.getState().retry(clientId);

    const msgs = useChat.getState().messages[convKey(acct)];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "e1", text: "hi", fromMe: true });
    expect(msgs[0].failed).toBeFalsy();
    expect(msgs[0].pending).toBeFalsy();
  });

  it("retry keeps the bubble failed (new reason) when the resend also fails", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "send_to_account")
        return Promise.reject({ kind: "service", message: "decrypt error" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);
    const clientId = useChat.getState().messages[convKey(acct)][0].clientId!;
    await useChat.getState().retry(clientId);

    const msgs = useChat.getState().messages[convKey(acct)];
    expect(msgs).toHaveLength(1);
    expect(msgs[0].failed).toBe(true);
    expect(msgs[0].failReason).toBe("crypto");
  });

  it("sets a structured error message when sendFile rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "send_file_to_account")
        return Promise.reject({ kind: "service", message: "disk full" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct, error: null });
    await useChat.getState().sendFile("/tmp/x", false);
    expect(useChat.getState().error).toContain("disk full");
  });

  it("sets a structured error message when toggleReaction rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "react_account")
        return Promise.reject({ kind: "service", message: "no peer" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct, error: null });
    await useChat.getState().toggleReaction("e1", "👍");
    expect(useChat.getState().error).toContain("no peer");
  });
});
