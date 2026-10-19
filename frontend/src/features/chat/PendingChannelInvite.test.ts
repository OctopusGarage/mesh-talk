// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { chatState } = vi.hoisted(() => ({
  chatState: {
    active: { kind: "channel", id: "first", name: "First" },
    acceptChannelInvite: vi.fn(async () => true),
    leaveChannel: vi.fn(async () => true),
  },
}));

vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (selector: (state: typeof chatState) => unknown) => selector(chatState),
    { getState: () => chatState },
  ),
}));
vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children, open }: { children: ReactNode; open: boolean }) =>
    open ? createElement("div", null, children) : null,
  DialogContent: ({ children }: { children: ReactNode }) =>
    createElement("div", null, children),
  DialogHeader: ({ children }: { children: ReactNode }) =>
    createElement("div", null, children),
  DialogTitle: ({ children }: { children: ReactNode }) =>
    createElement("h2", null, children),
  DialogDescription: ({ children }: { children: ReactNode }) =>
    createElement("p", null, children),
}));

import { PendingChannelInvite } from "./PendingChannelInvite";

it("does not carry a decline confirmation into another invitation", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(createElement(PendingChannelInvite, { channelId: "first" })),
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="pending-invite-decline-trigger"]',
        )!
        .click(),
    );
    expect(
      container.querySelector('[data-testid="pending-invite-decline-confirm"]'),
    ).not.toBeNull();

    chatState.active = { kind: "channel", id: "second", name: "Second" };
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="pending-invite-decline-confirm"]',
        )!
        .click(),
    );
    expect(chatState.leaveChannel).not.toHaveBeenCalled();
    await act(async () =>
      root.render(createElement(PendingChannelInvite, { channelId: "second" })),
    );
    expect(
      container.querySelector('[data-testid="pending-invite-decline-confirm"]'),
    ).toBeNull();
    expect(chatState.leaveChannel).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    chatState.active = { kind: "channel", id: "first", name: "First" };
    chatState.leaveChannel.mockClear();
  }
});

it("accepts and declines the current invitation", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(createElement(PendingChannelInvite, { channelId: "first" })),
    );
    await act(async () =>
      Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
        .find((button) => button.textContent === "Accept invitation")!
        .click(),
    );
    expect(chatState.acceptChannelInvite).toHaveBeenCalledOnce();

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="pending-invite-decline-trigger"]',
        )!
        .click(),
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="pending-invite-decline-confirm"]',
        )!
        .click(),
    );
    expect(chatState.leaveChannel).toHaveBeenCalledOnce();
    expect(
      container.querySelector('[data-testid="pending-invite-decline-confirm"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    chatState.acceptChannelInvite.mockClear();
    chatState.leaveChannel.mockClear();
  }
});
