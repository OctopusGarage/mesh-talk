// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { startUnreadBadge, useUnreadBadge } from "./unreadBadge";
import { useAuth } from "./auth";
import { useChat } from "./chat";

let stop: (() => void) | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset().mockResolvedValue(undefined);
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
  });
  useChat.setState({
    ready: true,
    unreadOwner: "host",
    myAccountId: "account",
    unread: { "channel:hidden": 2 },
    pendingUnknownUnread: { ghost: 1 },
  });
});
afterEach(() => {
  stop?.();
  stop = undefined;
  vi.useRealTimers();
});
it("paints all sources, reasserts on focus, and clears after the owning store changes", async () => {
  stop = startUnreadBadge("host");
  await vi.advanceTimersByTimeAsync(0);
  expect(invoke).toHaveBeenLastCalledWith(
    "set_badge",
    expect.objectContaining({ owner: "host", count: 3 }),
  );
  window.dispatchEvent(new Event("focus"));
  await vi.advanceTimersByTimeAsync(20);
  expect(invoke.mock.calls.length).toBeGreaterThan(1);
  useChat.setState({ unread: {}, pendingUnknownUnread: {} });
  await vi.advanceTimersByTimeAsync(0);
  expect(invoke).toHaveBeenLastCalledWith(
    "set_badge",
    expect.objectContaining({ count: 0 }),
  );
});
it("retired owner failures cannot update diagnostics or retry into the new session", async () => {
  let reject!: (error: Error) => void;
  invoke.mockImplementation(
    () =>
      new Promise((_, r) => {
        reject = r;
      }),
  );
  stop = startUnreadBadge("host");
  useAuth.setState({ generation: 1 });
  reject(new Error("old failure"));
  await vi.advanceTimersByTimeAsync(60_000);
  expect(useUnreadBadge.getState().error).toBeNull();
  expect(invoke).toHaveBeenCalledTimes(1);
});
