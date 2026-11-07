import { expect, it, vi } from "vitest";
import { createLinkedReadReceiptSync } from "./linkedReadReceipts";
import type { AccountDocumentPage } from "./api/accountDocuments";

const accountId = "b".repeat(32);
const key = `account:${accountId}`;
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
function fixture() {
  let active = true;
  let seen = { [key]: { local: { seen: true, late: false } } };
  const apply = vi.fn((key: string, ids: string[]) => {
    seen = {
      ...seen,
      [key]: {
        ...seen[key as keyof typeof seen],
        ...Object.fromEntries(
          ids.map((id) => [id, { seen: true, late: false }]),
        ),
      },
    };
  });
  const page: AccountDocumentPage = {
    items: [
      {
        key: {
          kind: "read_receipt",
          conversation: { kind: "account", id: accountId },
          message_id: "remote",
        },
        revision: 1,
        writer_device: "device",
        content_hash: [],
        content_len: 0,
        tombstone: false,
      },
    ],
    next: null,
    digest: [],
  };
  const ports = {
    current: () => active,
    ledger: () => seen,
    apply,
    subscribe: vi.fn(() => () => {}),
    policy: vi.fn().mockResolvedValue({ history: true }),
    list: vi.fn().mockResolvedValue(page),
    mark: vi.fn().mockResolvedValue(undefined),
  };
  return {
    ports,
    stopOwner: () => {
      active = false;
    },
    sync: createLinkedReadReceiptSync(ports),
  };
}
it("applies exact remote reads without echo and publishes only locally seen identities", async () => {
  const { ports, sync } = fixture();
  await sync.poll();
  expect(ports.apply).toHaveBeenCalledWith(key, ["remote"]);
  expect(ports.mark).not.toHaveBeenCalledWith(expect.anything(), "remote");
  expect(ports.mark).toHaveBeenCalledWith(
    { kind: "account", id: accountId },
    "local",
  );
  await sync.poll();
  expect(ports.apply).toHaveBeenCalledTimes(2);
});
it("history opt-out neither imports nor publishes receipts", async () => {
  const { ports, sync } = fixture();
  ports.policy.mockResolvedValue({ history: false });
  await sync.poll();
  expect(ports.list).not.toHaveBeenCalled();
  expect(ports.mark).not.toHaveBeenCalled();
  expect(ports.apply).not.toHaveBeenCalled();
});
it.each(["policy", "list"] as const)(
  "releases the receipt poll after a stalled %s read",
  async (lane) => {
    vi.useFakeTimers();
    const { ports, sync } = fixture();
    const stalled = deferred<never>();
    ports[lane].mockReturnValueOnce(stalled.promise);
    const completed = vi.fn();
    const polling = sync.poll().then(completed);
    try {
      await vi.advanceTimersByTimeAsync(10_001);
      expect(completed).toHaveBeenCalledOnce();
      await sync.poll();
      expect(ports[lane]).toHaveBeenCalledTimes(2);
      expect(ports.apply).toHaveBeenCalledWith(key, ["remote"]);
    } finally {
      sync.stop();
      stalled.resolve(undefined as never);
      await polling;
      vi.useRealTimers();
    }
  },
);
it("continues observing remote receipts while an original local write is outstanding", async () => {
  const { ports, sync } = fixture();
  const write = deferred<void>();
  ports.mark.mockReturnValueOnce(write.promise);
  const completed = vi.fn();
  const polling = sync.poll().then(completed);
  try {
    for (let i = 0; i < 12; i++) await Promise.resolve();
    expect(completed).toHaveBeenCalledOnce();
    await sync.poll();
    expect(ports.list).toHaveBeenCalledTimes(2);
    expect(ports.mark).toHaveBeenCalledOnce();
  } finally {
    sync.stop();
    write.resolve();
    await polling;
  }
});
it("rejects a delayed account page after runtime retirement", async () => {
  const { ports, sync, stopOwner } = fixture();
  const entered = deferred<void>();
  let resolve!: (page: AccountDocumentPage) => void;
  ports.list.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        resolve = yes;
        entered.resolve();
      }),
  );
  const pending = sync.poll();
  await entered.promise;
  stopOwner();
  resolve({ items: [], next: null, digest: [] });
  await pending;
  expect(ports.apply).not.toHaveBeenCalled();
  expect(ports.mark).not.toHaveBeenCalled();
});

it.each([false, true])(
  "never publishes confirmed local suppression as a read receipt (seen=%s)",
  async (seen) => {
    const { ports, sync } = fixture();
    const ledger = {
      [key]: { removed: { seen, late: false, suppressed: true } },
    };
    ports.ledger = () => ledger as unknown as ReturnType<typeof ports.ledger>;
    await sync.poll();
    expect(ports.mark).not.toHaveBeenCalledWith(expect.anything(), "removed");
  },
);

it("drains ready receipt pages without waiting for the recovery fallback", async () => {
  const { ports, sync } = fixture();
  const cursor = {
    kind: "read_receipt" as const,
    conversation: { kind: "account" as const, id: accountId },
    message_id: "first",
  };
  ports.list.mockResolvedValueOnce({ items: [], next: cursor, digest: [] });
  try {
    await sync.poll();
    expect(ports.list).toHaveBeenCalledTimes(2);
    expect(ports.list).toHaveBeenNthCalledWith(2, cursor);
    expect(ports.apply).toHaveBeenCalledWith(key, ["remote"]);
  } finally {
    sync.stop();
  }
});

it("bounds receipt catchup batches and cancels continuation with its owner", async () => {
  vi.useFakeTimers();
  const { ports, sync } = fixture();
  let page = 0;
  ports.list.mockImplementation(async () => ({
    items: [],
    digest: [],
    next: {
      kind: "read_receipt",
      conversation: { kind: "account", id: accountId },
      message_id: String(++page),
    },
  }));
  try {
    await sync.poll();
    expect(ports.list).toHaveBeenCalledTimes(8);
    expect(vi.getTimerCount()).toBe(1);
    sync.stop();
    await vi.advanceTimersByTimeAsync(1);
    expect(ports.list).toHaveBeenCalledTimes(8);
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    sync.stop();
    vi.useRealTimers();
  }
});

it("stops a nonadvancing receipt page instead of spinning", async () => {
  const { ports, sync } = fixture();
  const cursor = {
    kind: "read_receipt",
    conversation: { kind: "account", id: accountId },
    message_id: "same",
  };
  ports.list.mockResolvedValue({ items: [], next: cursor, digest: [] });
  try {
    await sync.poll();
    expect(ports.list).toHaveBeenCalledTimes(2);
  } finally {
    sync.stop();
  }
});

it("continues known local receipt work after exhausting a bounded write batch", async () => {
  vi.useFakeTimers();
  const { ports, sync } = fixture();
  ports.list.mockResolvedValue({ items: [], next: null, digest: [] });
  ports.ledger = () => ({
    [key]: {
      local: { seen: true, late: false },
      ...Object.fromEntries(
        Array.from({ length: 64 }, (_, i) => [
          `seen-${i}`,
          { seen: true, late: false },
        ]),
      ),
    },
  });
  try {
    await sync.poll();
    for (let i = 0; i < 200; i++) await Promise.resolve();
    expect(ports.mark).toHaveBeenCalledTimes(64);
    await vi.advanceTimersByTimeAsync(1);
    expect(ports.mark).toHaveBeenCalledTimes(65);
    sync.stop();
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    sync.stop();
    vi.useRealTimers();
  }
});

it("retains one receipt invalidation arriving during an active observation", async () => {
  vi.useFakeTimers();
  const { ports, sync } = fixture();
  const first = deferred<AccountDocumentPage>();
  ports.list.mockReturnValueOnce(first.promise);
  const observing = sync.poll();
  try {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    expect(ports.list).toHaveBeenCalledOnce();
    for (let i = 0; i < 5; i++) await sync.poll();
    expect(ports.list).toHaveBeenCalledOnce();
    first.resolve({ items: [], next: null, digest: [] });
    await observing;
    await vi.advanceTimersByTimeAsync(1);
    expect(ports.list).toHaveBeenCalledTimes(2);
    expect(ports.apply).toHaveBeenCalledWith(key, ["remote"]);
  } finally {
    first.resolve({ items: [], next: null, digest: [] });
    await observing;
    sync.stop();
    vi.useRealTimers();
  }
});

it("does not turn receipt observation or write failure into a hot retry", async () => {
  vi.useFakeTimers();
  const { ports, sync } = fixture();
  ports.list.mockRejectedValueOnce(new Error("offline"));
  ports.mark.mockRejectedValue(new Error("read only"));
  try {
    await sync.poll();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(ports.list).toHaveBeenCalledOnce();
    await sync.poll();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(ports.list).toHaveBeenCalledTimes(2);
    expect(ports.mark).toHaveBeenCalledOnce();
  } finally {
    sync.stop();
    vi.useRealTimers();
  }
});

it("ignores roster and delivery pulses but refreshes receipt documents on state", async () => {
  vi.useFakeTimers();
  const { startLinkedReadReceipts } = await import("./linkedReadReceipts");
  const { useAuth } = await import("@/store/auth");
  const { useChat } = await import("@/store/chat");
  const { chat } = await import("./api");
  const { notifySyncInvalidation } = await import("./syncInvalidation");
  const authBefore = useAuth.getState();
  const chatBefore = useChat.getState();
  useAuth.setState({
    user: { id: "receipt-owner", username: "owner", display_name: "Owner" },
  });
  useChat.setState({ ready: true, myAccountId: accountId });
  const policy = vi
    .spyOn(chat, "getDeviceSyncPolicy")
    .mockResolvedValue({ history: false } as Awaited<
      ReturnType<typeof chat.getDeviceSyncPolicy>
    >);
  const stop = startLinkedReadReceipts("receipt-owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(policy).toHaveBeenCalledTimes(1);
    for (let n = 0; n < 10; n++)
      for (const kind of ["roster", "delivery", "files"] as const)
        notifySyncInvalidation({ owner: "receipt-owner", kind });
    await vi.advanceTimersByTimeAsync(0);
    expect(policy).toHaveBeenCalledTimes(1);
    notifySyncInvalidation({ owner: "receipt-owner", kind: "state" });
    await vi.advanceTimersByTimeAsync(0);
    expect(policy).toHaveBeenCalledTimes(2);
  } finally {
    stop();
    policy.mockRestore();
    useAuth.setState(authBefore);
    useChat.setState(chatBefore);
    vi.useRealTimers();
  }
});
