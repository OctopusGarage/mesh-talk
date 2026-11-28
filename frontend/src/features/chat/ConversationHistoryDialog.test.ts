// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { historyApi, dialog } = vi.hoisted(() => ({
  historyApi: {
    ownerHistory: vi.fn(async () => [
      {
        id: "message-a",
        fromMe: false,
        who: "Alice",
        text: "private history from Alice",
        wallClock: 1,
        replyTo: null,
        file: null,
        recalled: false,
        recalledText: null,
        sticker: null,
      },
    ]),
    fetchChannelHistoryPage: vi.fn(
      async (_id: string, _cursor: string | null) => ({
        status: "page",
        applied: 0,
        cursor: "older" as string | null,
      }),
    ),
    channelHistory: vi.fn(async () => {
      throw new Error("offline");
    }),
  },
  dialog: {
    now: Date.now(),
    onOpenChange: null as null | ((open: boolean) => void),
  },
}));

vi.mock("@/lib/api", () => ({ chat: historyApi }));
vi.mock("@/store/chat", () => ({
  useChat: (
    selector: (state: { ready: boolean; expiryNow: number }) => unknown,
  ) => selector({ ready: true, expiryNow: dialog.now }),
  convKey: (conversation: { kind: string; id: string }) =>
    `${conversation.kind}:${conversation.id}`,
  captureChatOwnership: () => ({ owner: "me", current: () => true }),
  fromHistoryItem: (item: unknown) => item,
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileAvailability: Object.assign(
    (selector: (state: { statuses: Record<string, unknown> }) => unknown) =>
      selector({ statuses: {} }),
    { getState: () => ({ statuses: {} }) },
  ),
  watchFileAvailability: () => () => {},
}));
vi.mock("@/lib/savedDownloads", () => ({
  useSavedDownloads: () => ({}),
  rememberSavedDownload: () => {},
}));
vi.mock("./ClearHistoryButton", () => ({ ClearHistoryButton: () => null }));
vi.mock("./MediaPreview", () => ({ MediaPreview: () => null }));
vi.mock("./ReceiveProgress", () => ({ ReceiveProgress: () => null }));
vi.mock("@/components/ui/dialog", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: ({
      children,
      onOpenChange,
    }: {
      children: ReactNode;
      onOpenChange: (open: boolean) => void;
    }) => {
      dialog.onOpenChange = onOpenChange;
      return createElement("div", null, children);
    },
    DialogContent: Wrap,
    DialogHeader: Wrap,
    DialogTitle: Wrap,
    DialogDescription: Wrap,
    DialogTrigger: Wrap,
  };
});
vi.mock("@/components/ui/tabs", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Tabs: Wrap,
    TabsContent: Wrap,
    TabsList: Wrap,
    TabsTrigger: Wrap,
  };
});

import { ConversationHistoryDialog } from "./ConversationHistoryDialog";

it("does not show another conversation's rows after its history load fails", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(ConversationHistoryDialog, {
          conversation: { kind: "account", id: "alice", name: "Alice" },
        }),
      ),
    );
    await act(async () => dialog.onOpenChange?.(true));
    expect(container.textContent).toContain("private history from Alice");

    await act(async () =>
      root.render(
        createElement(ConversationHistoryDialog, {
          conversation: { kind: "channel", id: "group", name: "Group" },
        }),
      ),
    );
    expect(container.textContent).not.toContain("private history from Alice");
  } finally {
    await act(async () => root.unmount());
  }
});

it("removes an expired row from an already open history dialog", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  dialog.now = Date.now();
  const expiring = {
    id: "expiring",
    fromMe: false,
    who: "Alice",
    text: "temporary history",
    wallClock: 1,
    replyTo: null,
    file: null,
    recalled: false,
    recalledText: null,
    sticker: null,
    expiresAtMs: dialog.now + 60_000,
  };
  historyApi.ownerHistory.mockResolvedValueOnce([expiring]);
  const view = () =>
    createElement(ConversationHistoryDialog, {
      conversation: { kind: "account", id: "alice", name: "Alice" },
    });
  try {
    await act(async () => root.render(view()));
    await act(async () => dialog.onOpenChange?.(true));
    expect(container.textContent).toContain("temporary history");
    dialog.now += 60_001;
    await act(async () => root.render(view()));
    expect(container.textContent).not.toContain("temporary history");
  } finally {
    await act(async () => root.unmount());
  }
});

it("requests one recent page and only fetches older on an explicit click", async () => {
  historyApi.fetchChannelHistoryPage.mockClear();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(ConversationHistoryDialog, {
          conversation: { kind: "channel", id: "group", name: "Group" },
        }),
      ),
    );
    await act(async () => dialog.onOpenChange?.(true));
    expect(historyApi.fetchChannelHistoryPage.mock.calls).toEqual([
      ["group", null],
    ]);
    const button = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent === "Load older messages",
    )!;
    await act(async () => button.click());
    expect(historyApi.fetchChannelHistoryPage.mock.calls).toEqual([
      ["group", null],
      ["group", "older"],
    ]);
  } finally {
    await act(async () => root.unmount());
  }
});

it("does not retry preparing automatically or refresh after closing", async () => {
  historyApi.fetchChannelHistoryPage.mockClear();
  historyApi.fetchChannelHistoryPage.mockResolvedValueOnce({
    status: "preparing",
    applied: 0,
    cursor: null,
  });
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(ConversationHistoryDialog, {
          conversation: { kind: "channel", id: "group", name: "Group" },
        }),
      ),
    );
    await act(async () => dialog.onOpenChange?.(true));
    expect(historyApi.fetchChannelHistoryPage).toHaveBeenCalledTimes(1);
    let finish!: (page: {
      status: string;
      applied: number;
      cursor: string | null;
    }) => void;
    historyApi.fetchChannelHistoryPage.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const retry = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent === "Retry history request",
    )!;
    await act(async () => retry.click());
    await act(async () => dialog.onOpenChange?.(false));
    const reads = historyApi.channelHistory.mock.calls.length;
    await act(async () =>
      finish({ status: "page", applied: 1, cursor: "older" }),
    );
    expect(historyApi.channelHistory.mock.calls.length).toBe(reads);
    expect(historyApi.fetchChannelHistoryPage).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
  }
});

it("offers a fresh recent retry after navigating beyond a blocked gap", async () => {
  historyApi.fetchChannelHistoryPage.mockClear();
  historyApi.fetchChannelHistoryPage
    .mockResolvedValueOnce({
      status: "blocked",
      applied: 0,
      cursor: "older-gap",
    })
    .mockResolvedValueOnce({ status: "page", applied: 1, cursor: null })
    .mockResolvedValueOnce({ status: "page", applied: 1, cursor: null });
  const container = document.createElement("div");
  const root = createRoot(container);
  const click = async (label: string) =>
    act(async () =>
      Array.from(container.querySelectorAll("button"))
        .find((b) => b.textContent === label)!
        .click(),
    );
  try {
    await act(async () =>
      root.render(
        createElement(ConversationHistoryDialog, {
          conversation: { kind: "channel", id: "group", name: "Group" },
        }),
      ),
    );
    await act(async () => dialog.onOpenChange?.(true));
    expect(historyApi.fetchChannelHistoryPage.mock.calls).toEqual([
      ["group", null],
    ]);
    await click("Load older messages");
    expect(historyApi.fetchChannelHistoryPage.mock.calls).toEqual([
      ["group", null],
      ["group", "older-gap"],
    ]);
    expect(container.textContent).toContain("Retry recent messages");
    await click("Retry recent messages");
    expect(historyApi.fetchChannelHistoryPage.mock.calls).toEqual([
      ["group", null],
      ["group", "older-gap"],
      ["group", null],
    ]);
    expect(container.textContent).not.toContain("Retry recent messages");
  } finally {
    await act(async () => root.unmount());
  }
});
