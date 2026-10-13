// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { chatState, avatars, dialog } = vi.hoisted(() => ({
  chatState: {
    members: [
      {
        user_id: "owner-device",
        account_id: "owner-account",
        name: "Me",
        replica_enabled: true,
      },
    ],
    channelOwner: "owner-device",
    active: { kind: "channel", id: "channel", name: "Group" },
    peers: [
      {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        addr: "127.0.0.1",
        post_office: false,
      },
    ],
    favorites: {},
    myId: "owner-device",
    myAccountId: "owner-account",
    addMember: () => {},
    removeMember: () => {},
    dissolveChannel: vi.fn(async () => true),
    open: () => {},
  },
  avatars: { "peer-account": "data:image/png;base64,AAAA" } as Record<
    string,
    string
  >,
  dialog: { onOpenChange: null as null | ((open: boolean) => void) },
}));

vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
  displayName: (_favorites: unknown, _id: string, name: string) => name,
}));
vi.mock("@/store/auth", () => ({
  useAuth: (selector: (state: unknown) => unknown) =>
    selector({ user: { display_name: "Me" } }),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (selector: (state: unknown) => unknown) =>
    selector({ contacts: {}, loaded: true }),
}));
vi.mock("@/store/presence", () => ({
  usePresence: (selector: (state: unknown) => unknown) => selector({ map: {} }),
  OFFLINE: { online: false, last_seen_secs: 0 },
  presenceStatus: () => "offline",
}));
vi.mock("@/store/avatars", () => ({
  useAvatar: (id: string) => avatars[id],
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrapper = ({ children }: { children: ReactNode }) =>
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
    DialogContent: Wrapper,
    DialogHeader: Wrapper,
    DialogTitle: Wrapper,
    DialogDescription: Wrapper,
    DialogTrigger: Wrapper,
  };
});

import { MembersDialog } from "./MembersDialog";

it("shows an unjoined peer's account avatar in Add a peer", () => {
  const html = renderToStaticMarkup(createElement(MembersDialog));

  expect(html).toContain("Alice");
  expect(html).toContain('src="data:image/png;base64,AAAA"');
});

it("offers dissolution to the owner of an active channel", () => {
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain('data-testid="dissolve-channel-trigger"');
});

it("shows a member's signed replica opt-out", () => {
  const previous = chatState.members;
  chatState.members = [
    ...previous,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Alice",
      replica_enabled: false,
    },
  ];
  try {
    const html = renderToStaticMarkup(createElement(MembersDialog));
    expect(html).toContain('data-testid="member-replica-disabled-peer-device"');
  } finally {
    chatState.members = previous;
  }
});

it("does not offer dissolution after the channel is closed", () => {
  const html = renderToStaticMarkup(
    createElement(MembersDialog, { dissolved: true }),
  );
  expect(html).not.toContain('data-testid="dissolve-channel-trigger"');
});

it("requires confirmation and closes the dialog after successful dissolution", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  chatState.dissolveChannel.mockResolvedValue(true);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-trigger"]',
        )!
        .click(),
    );
    expect(container.textContent).toContain("Dissolve");
    expect(chatState.dissolveChannel).not.toHaveBeenCalled();
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-confirm"]',
        )!
        .click(),
    );
    expect(chatState.dissolveChannel).toHaveBeenCalledWith("channel");
    expect(
      container.querySelector('[data-testid="dissolve-channel-confirm"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    chatState.dissolveChannel.mockClear();
  }
});

it("keeps confirmation available after a failed dissolution", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  chatState.dissolveChannel.mockResolvedValue(false);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-trigger"]',
        )!
        .click(),
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-confirm"]',
        )!
        .click(),
    );
    expect(
      container.querySelector('[data-testid="dissolve-channel-confirm"]'),
    ).not.toBeNull();
    await act(async () =>
      Array.from(container.querySelectorAll("button"))
        .find((button) => button.textContent === "Cancel")!
        .click(),
    );
    expect(
      container.querySelector('[data-testid="dissolve-channel-trigger"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    chatState.dissolveChannel.mockClear();
  }
});

it("blocks closing confirmation while dissolution is pending", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  let finish!: (success: boolean) => void;
  chatState.dissolveChannel.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    act(() =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-trigger"]',
        )!
        .click(),
    );
    act(() =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-confirm"]',
        )!
        .click(),
    );
    act(() => dialog.onOpenChange!(false));
    expect(
      container.querySelector('[data-testid="dissolve-channel-confirm"]'),
    ).not.toBeNull();
    await act(async () => finish(false));
    act(() => dialog.onOpenChange!(false));
    expect(
      container.querySelector('[data-testid="dissolve-channel-trigger"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    chatState.dissolveChannel.mockClear();
  }
});
