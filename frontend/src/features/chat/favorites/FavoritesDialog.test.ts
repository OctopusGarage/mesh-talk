// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import type { MessageFavorite } from "@/lib/api/messageFavorites";
const bridge = vi.hoisted(() => ({
  invoke: vi.fn(),
  tasks: [] as Promise<unknown>[],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: bridge.invoke }));
vi.mock("@/components/ui/button", async (original) => {
  const actual = await original<typeof import("@/components/ui/button")>();
  const React = await import("react");
  return {
    ...actual,
    Button: React.forwardRef<
      HTMLButtonElement,
      React.ComponentProps<typeof actual.Button>
    >((props, ref) =>
      React.createElement(actual.Button, {
        ...props,
        ref,
        onClick: (event) => {
          const result: unknown = props.onClick?.(event);
          if (result instanceof Promise) bridge.tasks.push(result);
        },
      }),
    ),
  };
});
vi.mock("./FavoriteEditor", () => ({
  FavoriteEditor: ({ onClose }: { onClose: () => void }) =>
    createElement(
      "button",
      { onClick: onClose, "data-testid": "cancel-editor" },
      "Cancel",
    ),
}));
import { FavoritesDialog } from "./FavoritesDialog";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useMessageFavorites } from "@/store/messageFavorites";
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const favorite: MessageFavorite = {
  id: "favorite",
  source: { kind: "account", target: "bob", message_ids: ["m"] },
  source_label: "Bob",
  messages: [
    {
      id: "m",
      who: "Bob",
      sender_name: "Bob",
      text: "saved",
      wall_clock: 1,
      expires_at_ms: null,
      sticker: null,
      file: null,
    },
  ],
  metadata: { title: "Saved", description: "", notes: {} },
  saved_at_ms: 1,
  revision: 1,
  device_id: "device",
};
const response = {
  status: "found",
  reference: { kind: "account", target: "bob", message_id: "m" },
  messages: [
    { id: "m", who: "Bob", text: "saved", wall_clock: 1, from_me: false },
  ],
};
let root: Root, host: HTMLDivElement;
let transport: ReturnType<typeof deferred<typeof response>>,
  delivery: ReturnType<typeof deferred<void>>;
let open: ReturnType<typeof vi.fn<ReturnType<typeof useChat.getState>["open"]>>;
async function click(testId: string) {
  const element = document.querySelector<HTMLButtonElement>(
    `[data-testid="${testId}"]`,
  );
  expect(element).not.toBeNull();
  await act(async () => element!.click());
}
async function textClick(text: string) {
  const button = [...document.querySelectorAll("button")].find(
    (button) => button.textContent?.trim() === text,
  );
  expect(button, text).toBeDefined();
  await act(async () => button!.click());
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers();
  bridge.tasks = [];
  transport = deferred();
  delivery = deferred();
  bridge.invoke.mockReset();
  bridge.invoke.mockImplementation(async (command) => {
    if (command === "owner_search_context") {
      const result = await transport.promise;
      await delivery.promise;
      return result;
    }
    if (command === "owner_inspect_message_favorites")
      return { existing: favorite, saved_message_ids: ["m"] };
    if (command === "owner_list_message_favorites")
      return { items: [favorite], has_more: false };
    return null;
  });
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 3,
  });
  open = vi.fn<ReturnType<typeof useChat.getState>["open"]>(async () => {});
  useChat.setState({
    ready: true,
    runEpoch: 1,
    identityEpoch: 2,
    active: { kind: "channel", id: "team", name: "Team" },
    open,
  });
  useMessageFavorites.setState({
    items: [favorite],
    loading: false,
    error: null,
    hasMore: false,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(createElement(FavoritesDialog)));
  await click("open-message-favorites");
  await click("favorite-item");
});
afterEach(async () => {
  await act(async () => root.unmount());
  await act(async () => {
    transport.resolve(response);
    delivery.resolve();
    await Promise.all(bridge.tasks);
    vi.runOnlyPendingTimers();
  });
  host.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function startOriginal() {
  const before = bridge.tasks.length;
  await textClick("View original");
  expect(
    bridge.tasks.length,
    "the real handler must retain its complete operation promise",
  ).toBe(before + 1);
  return { pending: bridge.tasks[before] };
}
it.each(["close", "edit", "owner"])(
  "awaits the actual original-navigation consumer after %s retires it",
  async (mode) => {
    const { pending } = await startOriginal();
    let settled = false;
    void pending.finally(() => {
      settled = true;
    });
    if (mode === "close") {
      await textClick("Close");
      await act(async () =>
        useChat.setState({
          active: { kind: "channel", id: "replacement", name: "Replacement" },
        }),
      );
    } else if (mode === "edit") {
      await textClick("Edit notes");
      await click("cancel-editor");
      await click("favorite-item");
    } else await act(async () => useAuth.setState({ generation: 4 }));
    await act(async () => {
      transport.resolve(response);
      await vi.advanceTimersByTimeAsync(100);
    });
    expect(open).not.toHaveBeenCalled();
    expect(settled).toBe(false);
    await act(async () => {
      delivery.resolve();
      await pending;
    });
    expect(settled).toBe(true);
    expect(open).not.toHaveBeenCalled();
    if (mode === "close") {
      expect(
        document.querySelector('[data-testid="message-favorites"]'),
      ).toBeNull();
      expect(useChat.getState().active?.id).toBe("replacement");
    } else if (mode === "edit") {
      expect(
        document.querySelector('[data-testid="message-favorites"]'),
      ).not.toBeNull();
      const action = [...document.querySelectorAll("button")].find(
        (button) => button.textContent?.trim() === "View original",
      );
      expect(action?.disabled).toBe(false);
    } else expect(useAuth.getState().generation).toBe(4);
  },
);
it("retains the complete original task through real navigation before closing", async () => {
  const navigation = deferred<void>();
  open.mockImplementation(() => navigation.promise);
  const { pending } = await startOriginal();
  let settled = false;
  void pending.finally(() => {
    settled = true;
  });
  await act(async () => {
    transport.resolve(response);
    delivery.resolve();
  });
  expect(open).toHaveBeenCalledOnce();
  expect(open.mock.calls[0][0]).toMatchObject({ kind: "account", id: "bob" });
  expect(open.mock.calls[0][1]).toMatchObject({
    reference: response.reference,
  });
  expect(settled).toBe(false);
  expect(
    document.querySelector('[data-testid="message-favorites"]'),
  ).not.toBeNull();
  await act(async () => {
    navigation.resolve();
    await pending;
  });
  expect(settled).toBe(true);
  expect(
    document.querySelector('[data-testid="message-favorites"]'),
  ).toBeNull();
});
