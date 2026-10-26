// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import type { MessageFavorite } from "@/lib/api/messageFavorites";
const { inspect } = vi.hoisted(() => ({ inspect: vi.fn() }));
vi.mock("@/lib/api/messageFavorites", () => ({
  messageFavorites: { inspect },
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "alice", current: () => true }),
}));
vi.mock("./FavoriteEditor", () => ({ FavoriteEditor: () => null }));
import { useMessageCollection } from "./useMessageCollection";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
let current!: ReturnType<typeof useMessageCollection>;
function Harness({ scope }: { scope: string }) {
  current = useMessageCollection(
    { kind: "account", id: scope, name: scope },
    scope,
  );
  return null;
}
afterEach(() => vi.clearAllMocks());
it.each(["escape", "conversation round trip"])(
  "pending collect cannot reopen after %s",
  async (mode) => {
    let finish!: (value: {
      existing: MessageFavorite | null;
      saved_message_ids: string[];
    }) => void;
    inspect.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () =>
        root.render(createElement(Harness, { scope: "a" })),
      );
      let pending!: Promise<void>;
      await act(async () => {
        pending = current.collect([
          {
            id: "m1",
            fromMe: false,
            who: "Alice",
            text: "secret",
            wallClock: 1,
            replyTo: null,
          },
        ]);
      });
      if (mode === "escape")
        await act(async () =>
          document.dispatchEvent(
            new KeyboardEvent("keydown", { key: "Escape", cancelable: true }),
          ),
        );
      else {
        await act(async () =>
          root.render(createElement(Harness, { scope: "b" })),
        );
        await act(async () =>
          root.render(createElement(Harness, { scope: "a" })),
        );
      }
      await act(async () => {
        finish({ existing: null, saved_message_ids: [] });
        await pending;
      });
      expect(current.draft).toBeNull();
      expect(current.inspecting).toBe(false);
    } finally {
      await act(async () => root.unmount());
    }
  },
);

it.each(["click", "authenticate"] as const)(
  "rejects protected %s sources before inspection or snapshot preview",
  async (protection) => {
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () =>
        root.render(createElement(Harness, { scope: "a" })),
      );
      const protectedMessage = {
        id: "protected",
        fromMe: false,
        who: "Alice",
        text: "private text",
        wallClock: 1,
        replyTo: null,
        protection,
        file: {
          name: "private.png",
          size: 1,
          mime: "image/png",
          fileConv: "private-file",
          media: true,
        },
      };
      await act(async () => current.collect([protectedMessage]));
      expect(inspect).not.toHaveBeenCalled();
      expect(current.draft).toBeNull();
      const ordinary = {
        id: "ordinary",
        fromMe: false,
        who: "Alice",
        text: "public text",
        wallClock: 1,
        replyTo: null,
      };
      await act(async () => current.collect([ordinary, protectedMessage]));
      expect(inspect).not.toHaveBeenCalled();
      expect(current.draft).toBeNull();
    } finally {
      await act(async () => root.unmount());
    }
  },
);
