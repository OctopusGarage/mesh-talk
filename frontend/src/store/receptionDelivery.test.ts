import { afterEach, expect, it, vi } from "vitest";
import { createReceptionDelivery } from "./receptionDelivery";
import type {
  ReceptionCursor,
  ReceptionPage,
  ReceptionScope,
} from "@/lib/types";

const scope: ReceptionScope = {
  owner: "alice",
  account_id: "account",
  device_id: "device",
  runtime_id: "session:ticket",
};
const cursor = (n: number): ReceptionCursor => ({
  epoch: "epoch",
  sequence: String(n),
});
const page = (n: number, complete = true): ReceptionPage<string> => ({
  scope,
  token: `page-${n}`,
  after: cursor(n - 1),
  next: cursor(n),
  through: cursor(complete ? n : 3),
  complete,
  receipts: [`message-${n}`],
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
afterEach(() => vi.useRealTimers());

it("coalesces a stalled renderer wake burst into one pull and retains its dirty wake", async () => {
  const first = deferred<ReceptionPage<string>>();
  const pull = vi
    .fn()
    .mockReturnValueOnce(first.promise)
    .mockResolvedValueOnce(page(2));
  const apply = vi.fn();
  const persist = vi.fn();
  const acknowledge = vi.fn().mockResolvedValue(false);
  const delivery = createReceptionDelivery({
    scope,
    cursor: null,
    current: () => true,
    pull,
    apply,
    persist,
    acknowledge,
    failed: vi.fn(),
    yield: () => Promise.resolve(),
  });
  const running = delivery.wake();
  for (let n = 0; n < 10_000; n++) void delivery.wake();
  expect(pull).toHaveBeenCalledTimes(1);
  first.resolve(page(1));
  await running;
  expect(pull).toHaveBeenCalledTimes(2);
  expect(apply).toHaveBeenCalledTimes(2);
  expect(persist.mock.calls.map(([value]) => value)).toEqual([
    cursor(1),
    cursor(2),
  ]);
  delivery.stop();
});

it("persists current reception state before ACK and keeps the finite through cursor", async () => {
  const order: string[] = [];
  const pull = vi
    .fn()
    .mockResolvedValueOnce(page(1, false))
    .mockResolvedValueOnce(page(2, false))
    .mockResolvedValueOnce(page(3));
  const delivery = createReceptionDelivery({
    scope,
    cursor: cursor(0),
    current: () => true,
    pull,
    apply: (p) => {
      order.push(`apply:${p.next.sequence}`);
    },
    persist: (c) => {
      order.push(`persist:${c.sequence}`);
    },
    acknowledge: async (p) => {
      order.push(`ack:${p.next.sequence}`);
      return false;
    },
    failed: vi.fn(),
    yield: () => Promise.resolve(),
  });
  await delivery.wake();
  expect(order).toEqual([
    "apply:1",
    "persist:1",
    "ack:1",
    "apply:2",
    "persist:2",
    "ack:2",
    "apply:3",
    "persist:3",
    "ack:3",
  ]);
  expect(pull.mock.calls).toEqual([
    [cursor(0), null],
    [cursor(1), cursor(3)],
    [cursor(2), cursor(3)],
  ]);
  delivery.stop();
});

it("retries failed persistence without applying unread effects twice or ACKing unsaved state", async () => {
  vi.useFakeTimers();
  const failure = new Error("quota exhausted");
  const pull = vi.fn().mockResolvedValue(page(1));
  const apply = vi.fn();
  const persist = vi.fn().mockImplementationOnce(() => {
    throw failure;
  });
  const acknowledge = vi.fn().mockResolvedValue(false);
  const failed = vi.fn();
  const delivery = createReceptionDelivery({
    scope,
    cursor: null,
    current: () => true,
    pull,
    apply,
    persist,
    acknowledge,
    failed,
    retryMs: 1000,
  });
  await delivery.wake();
  expect(failed).toHaveBeenCalledWith(failure);
  expect(acknowledge).not.toHaveBeenCalled();
  await delivery.wake();
  expect(apply).toHaveBeenCalledTimes(1);
  expect(pull).toHaveBeenCalledTimes(1);
  expect(persist).toHaveBeenCalledTimes(2);
  expect(acknowledge).toHaveBeenCalledTimes(1);
  delivery.stop();
});

it("retries an ambiguous ACK without replaying applied or persisted receipt effects", async () => {
  vi.useFakeTimers();
  const pull = vi.fn().mockResolvedValue(page(1));
  const apply = vi.fn();
  const persist = vi.fn();
  const acknowledge = vi
    .fn()
    .mockRejectedValueOnce(new Error("response lost"))
    .mockResolvedValueOnce(false);
  const delivery = createReceptionDelivery({
    scope,
    cursor: null,
    current: () => true,
    pull,
    apply,
    persist,
    acknowledge,
    failed: vi.fn(),
  });
  await delivery.wake();
  await delivery.wake();
  expect(pull).toHaveBeenCalledTimes(1);
  expect(apply).toHaveBeenCalledTimes(1);
  expect(persist).toHaveBeenCalledTimes(1);
  expect(acknowledge).toHaveBeenCalledTimes(2);
  delivery.stop();
});

it("rejects stale runtime replies and stops pending retry work after replacement", async () => {
  const result = deferred<ReceptionPage<string>>();
  let current = true;
  const apply = vi.fn();
  const persist = vi.fn();
  const acknowledge = vi.fn().mockResolvedValue(false);
  const delivery = createReceptionDelivery({
    scope,
    cursor: null,
    current: () => current,
    pull: () => result.promise,
    apply,
    persist,
    acknowledge,
    failed: vi.fn(),
  });
  const running = delivery.wake();
  current = false;
  delivery.stop();
  result.resolve(page(1));
  await running;
  expect(apply).not.toHaveBeenCalled();
  expect(persist).not.toHaveBeenCalled();
  expect(acknowledge).not.toHaveBeenCalled();
});

it("never applies a page addressed to another account or installation", async () => {
  vi.useFakeTimers();
  const apply = vi.fn();
  const failed = vi.fn();
  const delivery = createReceptionDelivery({
    scope,
    cursor: null,
    current: () => true,
    pull: async () => ({
      ...page(1),
      scope: { ...scope, runtime_id: "replacement" },
    }),
    apply,
    persist: vi.fn(),
    acknowledge: vi.fn(),
    failed,
  });
  await delivery.wake();
  expect(apply).not.toHaveBeenCalled();
  expect(failed).toHaveBeenCalledTimes(1);
  delivery.stop();
});
