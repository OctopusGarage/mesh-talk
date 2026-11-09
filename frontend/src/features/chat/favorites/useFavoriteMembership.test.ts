// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatMessage } from "@/store/conversationModel";
import {
  notifySyncInvalidation,
  setPushStateSupport,
} from "@/lib/syncInvalidation";
const mock = vi.hoisted(() => ({ epoch: 0, inspect: vi.fn() }));
vi.mock("@/lib/api/messageFavorites", () => ({
  messageFavorites: { inspect: mock.inspect },
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => {
    const epoch = mock.epoch;
    return { owner: "alice", current: () => epoch === mock.epoch };
  },
}));
import { useFavoriteMembership } from "./useFavoriteMembership";
let root: Root;
let renders: number;
let ids: Set<string>;
const message = { id: "one" } as ChatMessage;
function Probe({
  scope = "first",
  messages = [message],
}: {
  scope?: string;
  messages?: ChatMessage[];
}) {
  ids = useFavoriteMembership(
    { kind: "account", id: "peer", name: "Peer" },
    messages,
    scope,
  );
  renders++;
  return null;
}
async function mount(scope = "first", messages = [message]) {
  await act(async () => root.render(createElement(Probe, { scope, messages })));
}
beforeEach(() => {
  vi.useFakeTimers();
  mock.epoch = 0;
  mock.inspect
    .mockReset()
    .mockResolvedValue({ saved_message_ids: ["one"], existing: null });
  setPushStateSupport("alice");
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  root = createRoot(document.createElement("div"));
  renders = 0;
});
afterEach(async () => {
  await act(async () => root.unmount());
  setPushStateSupport(null);
  vi.useRealTimers();
});
it("keeps quiet membership idle for five minutes and does not republish identical IDs", async () => {
  await mount();
  const baseline = renders;
  await act(async () => vi.advanceTimersByTimeAsync(299999));
  expect(mock.inspect).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(1));
  expect(mock.inspect).toHaveBeenCalledTimes(2);
  expect(renders).toBe(baseline);
});
it("refreshes owner state and foreground activation, ignoring other owners and roster", async () => {
  await mount();
  await act(async () => {
    notifySyncInvalidation({ owner: "other", kind: "state" });
    notifySyncInvalidation({ owner: "alice", kind: "roster" });
  });
  expect(mock.inspect).toHaveBeenCalledTimes(1);
  mock.inspect.mockResolvedValue({ saved_message_ids: [], existing: null });
  await act(async () =>
    notifySyncInvalidation({ owner: "alice", kind: "state" }),
  );
  expect(ids.size).toBe(0);
  await act(async () => window.dispatchEvent(new Event("focus")));
  expect(mock.inspect).toHaveBeenCalledTimes(3);
});
it("parks hidden recovery and resumes on return", async () => {
  await mount();
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "hidden",
  });
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  await act(async () => vi.advanceTimersByTimeAsync(600000));
  expect(mock.inspect).toHaveBeenCalledTimes(1);
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  expect(mock.inspect).toHaveBeenCalledTimes(2);
});
it("rejects retired owner results and coalesces pending invalidations", async () => {
  let resolve!: (value: { saved_message_ids: string[] }) => void;
  mock.inspect.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  await mount();
  await act(async () => {
    notifySyncInvalidation({ owner: "alice", kind: "state" });
    notifySyncInvalidation({ owner: "alice", kind: "state" });
  });
  expect(mock.inspect).toHaveBeenCalledTimes(1);
  await act(async () => resolve({ saved_message_ids: [] }));
  expect(mock.inspect).toHaveBeenCalledTimes(2);
  expect(ids.has("one")).toBe(true);
  mock.inspect.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  await act(async () =>
    notifySyncInvalidation({ owner: "alice", kind: "state" }),
  );
  mock.epoch++;
  await mount("replacement");
  await act(async () => resolve({ saved_message_ids: [] }));
  expect(ids.has("one")).toBe(true);
});
it("refreshes at an actual message expiry", async () => {
  await mount("first", [{ ...message, expiresAtMs: Date.now() + 5000 }]);
  mock.inspect.mockResolvedValue({ saved_message_ids: [], existing: null });
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(ids.size).toBe(0);
  expect(mock.inspect).toHaveBeenCalledTimes(2);
});

it("recovers a failed inspection on activation without clearing known membership", async () => {
  await mount();
  mock.inspect.mockRejectedValueOnce(new Error("offline"));
  await act(async () =>
    notifySyncInvalidation({ owner: "alice", kind: "state" }),
  );
  expect(ids.has("one")).toBe(true);
  mock.inspect.mockResolvedValue({ saved_message_ids: [], existing: null });
  await act(async () => window.dispatchEvent(new Event("focus")));
  expect(ids.size).toBe(0);
});
