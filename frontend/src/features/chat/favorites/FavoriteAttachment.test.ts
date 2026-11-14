// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: bridge.invoke }));
import { FavoriteAttachment } from "./FavoriteAttachment";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { notifySyncInvalidation } from "@/lib/syncInvalidation";
const file = {
  file_conv: "retained",
  name: "saved.png",
  size: 1024,
  mime: "image/png",
  media: true,
};
let root: Root, host: HTMLDivElement, mounted: boolean;
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const reads = () =>
  bridge.invoke.mock.calls.filter(
    ([command]) => command === "owner_favorite_file_available",
  ).length;
const saveButton = () =>
  [...host.querySelectorAll("button")].find(
    (button) => button.textContent?.trim() === "Save",
  )!;
async function mount() {
  await act(async () =>
    root.render(createElement(FavoriteAttachment, { file })),
  );
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers();
  bridge.invoke.mockReset();
  bridge.invoke.mockImplementation(async (command) => {
    if (command === "owner_favorite_file_available") return false;
    throw new Error(`Unexpected native command: ${command}`);
  });
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 3,
  });
  useChat.setState({ ready: true, runEpoch: 1, identityEpoch: 2 });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  mounted = true;
});
afterEach(async () => {
  if (mounted) await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
it("keeps settled missing-file availability quiet before sparse repair", async () => {
  await mount();
  expect(reads()).toBe(1);
  expect(saveButton().disabled).toBe(true);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  expect(reads()).toBe(1);
});
it.each(["documents", "files", "focus", "visibility"])(
  "refreshes retained-file authority promptly on %s",
  async (kind) => {
    let available = false;
    bridge.invoke.mockImplementation(async () => available);
    await mount();
    expect(saveButton().disabled).toBe(true);
    await act(async () =>
      notifySyncInvalidation({
        owner: "alice",
        kind: "state",
        scope: "sync_status",
      }),
    );
    expect(reads()).toBe(1);
    available = true;
    await act(async () => {
      if (kind === "documents")
        notifySyncInvalidation({
          owner: "alice",
          kind: "state",
          scope: "documents",
        });
      else if (kind === "files")
        notifySyncInvalidation({ owner: "alice", kind: "files" });
      else if (kind === "focus") window.dispatchEvent(new Event("focus"));
      else document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(reads()).toBe(2);
    expect(saveButton().disabled).toBe(false);
  },
);
it("coalesces file arrivals during a held authority read without overlapping requests", async () => {
  const held = deferred<boolean>();
  let requests = 0,
    active = 0,
    peak = 0;
  bridge.invoke.mockImplementation(() => {
    requests++;
    active++;
    peak = Math.max(peak, active);
    return (requests === 1 ? held.promise : Promise.resolve(true)).finally(
      () => {
        active--;
      },
    );
  });
  await mount();
  expect(reads()).toBe(1);
  expect(saveButton().disabled).toBe(true);
  await act(async () => {
    notifySyncInvalidation({ owner: "alice", kind: "files" });
    notifySyncInvalidation({ owner: "alice", kind: "files" });
  });
  expect(reads()).toBe(1);
  await act(async () => held.resolve(false));
  expect(reads()).toBe(2);
  expect(peak).toBe(1);
  expect(saveButton().disabled).toBe(false);
});
it.each(["owner", "unmount"])(
  "refuses a held availability result after %s retirement",
  async (retire) => {
    const held = deferred<boolean>();
    bridge.invoke.mockReturnValue(held.promise);
    await mount();
    expect(reads()).toBe(1);
    if (retire === "owner")
      await act(async () => useAuth.setState({ generation: 4 }));
    else {
      await act(async () => root.unmount());
      mounted = false;
    }
    await act(async () => held.resolve(true));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300_001);
    });
    expect(reads()).toBe(1);
    if (retire === "owner") expect(saveButton().disabled).toBe(true);
    else expect(host.children).toHaveLength(0);
  },
);
it("does not enable protected file actions when the authoritative read fails", async () => {
  bridge.invoke.mockRejectedValue(
    new Error("Current favorite permission denied"),
  );
  await mount();
  expect(saveButton().disabled).toBe(true);
  expect(host.textContent).toContain("Current favorite permission denied");
  expect(
    bridge.invoke.mock.calls.every(
      ([command]) => command === "owner_favorite_file_available",
    ),
  ).toBe(true);
});
