// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { chatState } = vi.hoisted(() => ({
  chatState: {
    active: { kind: "channel", id: "first", name: "First" },
    acceptChannelInvite: vi.fn(async (): Promise<unknown> => ({
      kind: "committed",
    })),
    leaveChannel: vi.fn(async () => ({
      kind: "pending",
      id: "pending-op",
      status: { phase: { kind: "waiting_for_executor" } },
    })),
    myAccountId: "account",
    runEpoch: 1,
    identityEpoch: 1,
    groupOperations: {},
    cancelGroupOperation: vi.fn(),
    declineChannelInvite: vi.fn(async (): Promise<unknown> => ({
      kind: "committed",
    })),
  },
}));

vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (selector: (state: typeof chatState) => unknown) => selector(chatState),
    { getState: () => chatState },
  ),
}));
vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({
    children,
    open,
    onOpenChange,
  }: {
    children: ReactNode;
    open: boolean;
    onOpenChange(open: boolean): void;
  }) =>
    open
      ? createElement(
          "div",
          null,
          createElement(
            "button",
            {
              "data-testid": "dialog-dismiss",
              onClick: () => onOpenChange(false),
            },
            "Dismiss",
          ),
          children,
        )
      : null,
  DialogContent: ({ children }: { children: ReactNode }) =>
    createElement("div", null, children),
  DialogHeader: ({ children }: { children: ReactNode }) =>
    createElement("div", null, children),
  DialogTitle: ({ children }: { children: ReactNode }) =>
    createElement("h2", null, children),
  DialogDescription: ({ children }: { children: ReactNode }) =>
    createElement("p", null, children),
}));

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

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
    expect(chatState.declineChannelInvite).not.toHaveBeenCalled();
    await act(async () =>
      root.render(createElement(PendingChannelInvite, { channelId: "second" })),
    );
    expect(
      container.querySelector('[data-testid="pending-invite-decline-confirm"]'),
    ).toBeNull();
    expect(chatState.declineChannelInvite).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    chatState.active = { kind: "channel", id: "first", name: "First" };
    chatState.declineChannelInvite.mockClear();
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
    expect(chatState.declineChannelInvite).toHaveBeenCalledOnce();
    expect(
      container.querySelector('[data-testid="pending-invite-decline-confirm"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    chatState.acceptChannelInvite.mockClear();
    chatState.declineChannelInvite.mockClear();
  }
});

it.each(["waiting_for_executor", "cancellation_requested"])(
  "allows dismissing the dialog while retaining the %s decline status",
  async (phase) => {
    const container = document.createElement("div");
    const root = createRoot(container);
    chatState.declineChannelInvite.mockResolvedValueOnce({
      kind: "pending",
      id: "pending-op",
      status: { phase: { kind: phase } },
    });
    try {
      await act(async () =>
        root.render(
          createElement(PendingChannelInvite, { channelId: "first" }),
        ),
      );
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
      expect(
        container.querySelector(
          '[data-testid="pending-invite-decline-confirm"]',
        ),
      ).not.toBeNull();
      await act(async () =>
        container
          .querySelector<HTMLButtonElement>('[data-testid="dialog-dismiss"]')!
          .click(),
      );
      expect(
        container.querySelector(
          '[data-testid="pending-invite-decline-confirm"]',
        ),
      ).toBeNull();
      expect(container.textContent).toContain(
        phase === "cancellation_requested"
          ? "Cancellation requested"
          : "your linked devices",
      );
    } finally {
      await act(async () => root.unmount());
    }
  },
);
