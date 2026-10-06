import { beforeEach, expect, it, vi } from "vitest";

const { listen } = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { subscribeNodeEvents } from "./events";

beforeEach(() => listen.mockReset());

it("forwards only subscribed event payloads and releases every listener", async () => {
  const subscriptions = new Map<
    string,
    (event: { payload: unknown }) => void
  >();
  const unlisten = vi.fn();
  listen.mockImplementation((name, callback) => {
    subscriptions.set(name, callback);
    return Promise.resolve(unlisten);
  });
  const onDm = vi.fn();
  const onFileProgress = vi.fn();
  const stop = subscribeNodeEvents({ onDm, onFileProgress });

  expect([...subscriptions.keys()]).toEqual(["dm-received", "file-progress"]);
  const message = { text: "hello" };
  subscriptions.get("dm-received")?.({ payload: message });
  subscriptions.get("file-progress")?.({ payload: { done: 2 } });
  expect(onDm).toHaveBeenCalledWith(message);
  expect(onFileProgress).toHaveBeenCalledWith({ done: 2 });

  stop();
  await vi.waitFor(() => expect(unlisten).toHaveBeenCalledTimes(2));
});

it("releases a listener even when registration resolves after cleanup", async () => {
  let finish!: (fn: () => void) => void;
  const unlisten = vi.fn();
  listen.mockReturnValue(new Promise((resolve) => (finish = resolve)));
  const stop = subscribeNodeEvents({ onProfile: vi.fn() });

  stop();
  finish(unlisten);
  await vi.waitFor(() => expect(unlisten).toHaveBeenCalledOnce());
});
