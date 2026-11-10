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
