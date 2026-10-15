import { expect, it, vi } from "vitest";
import { createSettingSaveQueue } from "./settingSaveQueue";

it("runs a second setting save after the first and stays busy until both finish", async () => {
  let finishFirst!: () => void;
  const first = new Promise<void>((resolve) => {
    finishFirst = resolve;
  });
  const busy = vi.fn();
  const order: string[] = [];
  const queue = createSettingSaveQueue(busy);
  const one = queue.enqueue(
    async () => {
      order.push("write one");
      await first;
    },
    () => order.push("apply one"),
    vi.fn(),
  );
  const two = queue.enqueue(
    async () => {
      order.push("write two");
    },
    () => order.push("apply two"),
    vi.fn(),
  );
  let idle = false;
  const drained = queue.whenIdle().then(() => {
    idle = true;
  });
  await vi.waitFor(() => expect(order).toEqual(["write one"]));
  expect(busy).toHaveBeenLastCalledWith(true);
  expect(idle).toBe(false);
  finishFirst();
  await Promise.all([one, two, drained]);
  expect(idle).toBe(true);
  expect(order).toEqual(["write one", "apply one", "write two", "apply two"]);
  expect(busy).toHaveBeenLastCalledWith(false);
});

it("continues after a failed save without applying the failed value", async () => {
  const error = vi.fn();
  const apply = vi.fn();
  const queue = createSettingSaveQueue(vi.fn());
  await queue.enqueue(
    async () => {
      throw new Error("offline");
    },
    apply,
    error,
  );
  await queue.enqueue(async () => {}, apply, error);
  expect(error).toHaveBeenCalledTimes(1);
  expect(apply).toHaveBeenCalledTimes(1);
});

it("publishes the saved read-back value only after the write succeeds", async () => {
  const published: number[] = [];
  const queue = createSettingSaveQueue(vi.fn());
  await queue.enqueue(
    async () => 42,
    (value) => published.push(value),
    vi.fn(),
  );
  expect(published).toEqual([42]);
});
