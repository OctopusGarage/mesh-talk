// @vitest-environment jsdom
import { act, createElement, type HTMLAttributes, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import type {
  ChannelActivityInfo,
  ChannelMemberInfo,
  ChannelMembersInfo,
  ChannelInfo,
} from "@/lib/types";
import "@/lib/i18n";

const { chatState, authState, avatars, dialog, channelApi, bindingChanges } =
  vi.hoisted(() => ({
    authState: {
      user: { id: "local-owner", username: "me", display_name: "Me" },
      generation: 1,
    },
    bindingChanges: vi.fn(async (owner: string) => ({ owner, changes: [] })),
    chatState: {
      runEpoch: 1,
      identityEpoch: 1,
      groupOperations: {} as Record<
        string,
        import("@/store/accountGroupOperations").GroupOperationTicket
      >,
      cancelGroupOperation: vi.fn(async () => ({ kind: "pending" })),
      refreshGroupOperations: vi.fn(async () => {}),
      refreshRoster: vi.fn(async () => {}),
      accounts: [] as Array<{
        account_id: string;
        device_count: number;
        names: string[];
      }>,
      members: [
        {
          user_id: "owner-device",
          account_id: "owner-account",
          name: "Me",
          replica_enabled: true,
        },
      ] as ChannelMemberInfo[],
      channels: [] as ChannelInfo[],
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
      addMember: vi.fn(async (): Promise<unknown> => ({ kind: "committed" })),
      removeMemberAccount: vi.fn(async (): Promise<unknown> => ({
        kind: "committed",
      })),
      transferChannelOwner: vi.fn(async (): Promise<unknown> => ({
        kind: "committed",
      })),
      removeMember: vi.fn(async (): Promise<unknown> => ({
        kind: "committed",
      })),
      dissolveChannel: vi.fn(async (): Promise<unknown> => ({
        kind: "committed",
      })),
      open: vi.fn(),
    },
    avatars: { "peer-account": "data:image/png;base64,AAAA" } as Record<
      string,
      string
    >,
    dialog: { onOpenChange: null as null | ((open: boolean) => void) },
    channelApi: {
      channelMembers: vi.fn(async (): Promise<ChannelMembersInfo> => ({
        owner: "owner-device",
        history_policy: "off",
        members: [
          {
            user_id: "owner-device",
            account_id: "owner-account",
            name: "Me",
            replica_enabled: true,
          },
        ],
      })),
      channelActivity: vi.fn(
        async (_channelId: string): Promise<ChannelActivityInfo[]> => [],
      ),
      setChannelReplicaEnabled: vi.fn(async () => {}),
      setGroupFileSharingOverride: vi.fn(async () => {}),
    },
  }));

vi.mock("@/lib/api", () => ({ chat: channelApi }));
vi.mock("@/lib/api/privacy", () => ({ privacy: { bindingChanges } }));

vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
  displayName: (_favorites: unknown, _id: string, name: string) => name,
}));
vi.mock("@/store/auth", () => ({
  useAuth: Object.assign(
    (selector: (state: typeof authState) => unknown) => selector(authState),
    {
      getState: () => authState,
      subscribe: () => () => {},
    },
  ),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (selector: (state: unknown) => unknown) =>
    selector({ contacts: {}, loaded: true }),
}));
vi.mock("@/store/presence", () => ({
  usePresence: (selector: (state: unknown) => unknown) => selector({ map: {} }),
  usePresenceFor: () => ({ online: false, last_seen_secs: null }),
  OFFLINE: { online: false, last_seen_secs: 0 },
  presenceStatus: () => "offline",
  presenceLabel: () => "Offline",
}));
vi.mock("@/store/avatars", () => ({
  useAvatar: (id: string) => avatars[id],
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrapper = ({
    children,
    asChild: _asChild,
    ...props
  }: HTMLAttributes<HTMLDivElement> & {
    children: ReactNode;
    asChild?: boolean;
  }) => createElement("div", props, children);
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

const defaultMembers = chatState.members.map((member) => ({ ...member }));
const defaultPeers = chatState.peers.map((peer) => ({ ...peer }));
const defaultActive = { ...chatState.active };

beforeEach(() => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  chatState.groupOperations = {};
  chatState.cancelGroupOperation.mockClear();
  chatState.refreshGroupOperations.mockClear();
  chatState.members = defaultMembers.map((member) => ({ ...member }));
  chatState.channelOwner = "owner-device";
  chatState.channels = [];
  chatState.active = { ...defaultActive };
  chatState.peers = defaultPeers.map((peer) => ({ ...peer }));
  chatState.favorites = {};
  chatState.accounts = [];
  chatState.myId = "owner-device";
  chatState.myAccountId = "owner-account";
  chatState.addMember.mockReset().mockResolvedValue({ kind: "committed" });
  chatState.removeMemberAccount
    .mockReset()
    .mockResolvedValue({ kind: "committed" });
  chatState.removeMember.mockReset().mockResolvedValue({ kind: "committed" });
  chatState.transferChannelOwner
    .mockReset()
    .mockResolvedValue({ kind: "committed" });
  chatState.dissolveChannel
    .mockReset()
    .mockResolvedValue({ kind: "committed" });
  chatState.open.mockReset();
  bindingChanges.mockClear();
  dialog.onOpenChange = null;
  channelApi.channelMembers.mockReset().mockImplementation(async () => ({
    owner: "owner-device",
    history_policy: "off",
    members: defaultMembers.map((member) => ({ ...member })),
  }));
  channelApi.channelActivity.mockReset().mockResolvedValue([]);
  channelApi.setChannelReplicaEnabled.mockReset().mockResolvedValue(undefined);
  channelApi.setGroupFileSharingOverride
    .mockReset()
    .mockResolvedValue(undefined);
});

it("projects three signed devices as two people with one stable owner badge", () => {
  chatState.members = [
    {
      user_id: "linked-device",
      account_id: "owner-account",
      name: "Messi10",
      replica_enabled: true,
    },
    {
      user_id: "owner-device",
      account_id: "owner-account",
      name: "Old name",
      replica_enabled: true,
    },
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Ronaldo",
      replica_enabled: true,
    },
  ];
  chatState.accounts = [
    {
      account_id: "owner-account",
      names: ["Canonical owner"],
      device_count: 2,
    },
    { account_id: "peer-account", names: ["Ronaldo"], device_count: 1 },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html.match(/data-member-row=/g) ?? []).toHaveLength(2);
  expect(html.match(/data-testid="member-owner-badge"/g) ?? []).toHaveLength(1);
  expect(html).toContain("Canonical owner");
  expect(html).not.toContain("Messi10");
  expect(html).not.toContain('data-testid="member-dm-linked-device"');
  expect(html).toContain('data-testid="member-device-linked-device"');
});

it("keeps unbound devices separate even when their display names match", () => {
  chatState.members = [
    {
      user_id: "unknown-a",
      account_id: null,
      name: "Same",
      replica_enabled: true,
    },
    {
      user_id: "unknown-b",
      account_id: null,
      name: "Same",
      replica_enabled: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html.match(/data-member-row=/g) ?? []).toHaveLength(2);
});

it("removes an account through one atomic action and transfers to a verified member account", async () => {
  chatState.members = [
    ...defaultMembers,
    { user_id: "peer-device", account_id: "peer-account", name: "Alice" },
    {
      user_id: "peer-linked",
      account_id: "peer-account",
      name: "Old device name",
    },
    {
      user_id: "peer-pending",
      account_id: "peer-account",
      name: "Pending",
      pending: true,
    },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.querySelectorAll("[data-member-row]")).toHaveLength(2);
    expect(
      container.querySelectorAll('[data-testid^="member-dm-"]'),
    ).toHaveLength(1);
    expect(
      container
        .querySelector('[data-testid="member-device-peer-pending"]')
        ?.querySelector('[title="Transfer ownership"]'),
    ).toBeNull();
    const transfer = container.querySelector<HTMLButtonElement>(
      '[data-testid="member-transfer-account-peer-account"]',
    )!;
    await act(async () => transfer.click());
    const transferConfirm = Array.from(
      container.querySelectorAll("button"),
    ).find((button) => button.textContent === "Transfer ownership")!;
    await act(async () => transferConfirm.click());
    expect(chatState.transferChannelOwner).toHaveBeenCalledExactlyOnceWith(
      "peer-account",
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-remove-account-peer-account"]',
        )!
        .click(),
    );
    const remove = Array.from(container.querySelectorAll("button")).find(
      (button) => button.textContent === "Remove member",
    )!;
    await act(async () => remove.click());
    expect(chatState.removeMemberAccount).toHaveBeenCalledExactlyOnceWith(
      "peer-account",
    );
    expect(chatState.removeMember).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});

it("does not grant owner controls to another device of the owner's account", () => {
  chatState.members = [
    ...defaultMembers,
    {
      user_id: "my-linked-device",
      account_id: "owner-account",
      name: "Other name",
    },
  ];
  chatState.myId = "my-linked-device";
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html.match(/data-member-row=/g) ?? []).toHaveLength(1);
  expect(html).toContain('data-testid="member-owner-badge"');
  expect(html).not.toContain('data-testid="dissolve-channel-trigger"');
  expect(html).not.toContain('title="Transfer ownership"');
  expect(html).not.toContain(
    'data-testid="member-remove-account-owner-account"',
  );
});

it("deduplicates invite people while exposing additional devices under existing accounts", async () => {
  chatState.peers = [
    ...defaultPeers,
    { ...defaultPeers[0], user_id: "peer-linked" },
    {
      ...defaultPeers[0],
      user_id: "owner-linked",
      account_id: "owner-account",
      name: "Duplicate self",
    },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    expect(
      container.querySelectorAll('[data-testid^="member-invite-"]'),
    ).toHaveLength(1);
    expect(
      container.querySelector('[data-testid="member-add-device-owner-linked"]'),
    ).not.toBeNull();
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-add-device-owner-linked"]',
        )!
        .click(),
    );
    expect(chatState.addMember).toHaveBeenCalledExactlyOnceWith("owner-linked");
  } finally {
    await act(async () => root.unmount());
  }
});

it("shows an unjoined peer's account avatar in Add a peer", () => {
  const html = renderToStaticMarkup(createElement(MembersDialog));

  expect(html).toContain("Alice");
  expect(html).toContain('src="data:image/png;base64,AAAA"');
});

it("keeps post office nodes out of the Add a peer picker", () => {
  chatState.peers = [
    ...chatState.peers,
    {
      user_id: "relay-device",
      account_id: "relay-account",
      name: "mac2015-post-office",
      addr: "100.64.0.1",
      post_office: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain("Alice");
  expect(html).not.toContain("mac2015-post-office");
});

it("offers dissolution to the owner of an active channel", () => {
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain('data-testid="dissolve-channel-trigger"');
});

it("shows a member's signed replica opt-out", () => {
  chatState.members = [
    ...chatState.members,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Alice",
      replica_enabled: false,
    },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain('data-testid="member-replica-disabled-peer-device"');
});

it("shows a newer store membership after the dialog's initial fetch", async () => {
  const previous = chatState.members;
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    expect(
      container.querySelector('[data-testid="member-profile-peer-device"]'),
    ).toBeNull();

    chatState.members = [
      ...previous,
      {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        replica_enabled: true,
      },
    ];
    await act(async () => root.render(createElement(MembersDialog)));
    expect(
      container.querySelector('[data-testid="member-profile-peer-device"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
  }
});

it("ignores a fetch started before a membership update", async () => {
  const previous = chatState.members;
  let finish!: (members: typeof previous) => void;
  channelApi.channelMembers.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = (members) =>
          resolve({ owner: "owner-device", history_policy: "off", members });
      }),
  );
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    chatState.members = [
      ...previous,
      {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        replica_enabled: true,
      },
    ];
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => finish(previous));
    expect(
      container.querySelector('[data-testid="member-profile-peer-device"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
  }
});

it("uses the fetched owner with the fetched member list", async () => {
  channelApi.channelMembers.mockResolvedValue({
    owner: "peer-device",
    history_policy: "off",
    members: [
      ...chatState.members,
      {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        replica_enabled: true,
      },
    ],
  });
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    expect(
      container.querySelector('[data-testid="dissolve-channel-trigger"]'),
    ).toBeNull();
    expect(
      container.querySelector('[data-testid="member-profile-peer-device"]')
        ?.parentElement?.textContent,
    ).toContain("Owner");
  } finally {
    await act(async () => root.unmount());
  }
});

it("opens a member's profile without removing the direct-message action", async () => {
  const previous = chatState.members;
  chatState.members = [
    ...previous,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Alice",
      replica_enabled: true,
    },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-profile-peer-device"]',
        )!
        .click(),
    );
    expect(
      container.querySelector('[data-testid="member-profile-dialog"]'),
    ).not.toBeNull();
    expect(container.textContent).toContain("peer-account");
    expect(container.textContent).toContain("127.0.0.1");
    expect(bindingChanges).toHaveBeenCalledWith("local-owner", "peer-account");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-dm-peer-device"]',
        )!
        .click(),
    );
    expect(chatState.open).toHaveBeenCalledWith({
      kind: "account",
      id: "peer-account",
      name: "Alice",
    });
  } finally {
    await act(async () => root.unmount());
  }
});

it("loads live replica state, toggles the local replica, and preserves it on a failed request", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    expect(channelApi.channelMembers).toHaveBeenCalledWith("channel");
    expect(channelApi.channelActivity).toHaveBeenCalledWith("channel");

    const toggle = () =>
      Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
        (button) => button.textContent?.includes("sharing"),
      )!;
    await act(async () => toggle().click());
    expect(channelApi.setChannelReplicaEnabled).toHaveBeenCalledWith(
      "channel",
      false,
    );
    expect(
      container.querySelector(
        '[data-testid="member-replica-disabled-owner-device"]',
      ),
    ).not.toBeNull();

    channelApi.setChannelReplicaEnabled.mockRejectedValueOnce(
      new Error("offline"),
    );
    await act(async () => toggle().click());
    expect(channelApi.setChannelReplicaEnabled).toHaveBeenLastCalledWith(
      "channel",
      true,
    );
    expect(
      container.querySelector(
        '[data-testid="member-replica-disabled-owner-device"]',
      ),
    ).not.toBeNull();
    expect(container.textContent).toContain("Could not update");
  } finally {
    await act(async () => root.unmount());
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
  chatState.dissolveChannel.mockResolvedValue({ kind: "committed" });
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
  }
});

it("keeps confirmation available after a failed dissolution", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  chatState.dissolveChannel.mockResolvedValue({
    kind: "not_admitted",
    error: new Error("not authorized"),
  });
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
  }
});

it("does not carry a removal confirmation into another channel", async () => {
  const previousMembers = chatState.members;
  chatState.members = [
    ...previousMembers,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Alice",
      replica_enabled: true,
    },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[aria-label="Remove Alice"]')!
        .click(),
    );
    expect(container.textContent).toContain("Remove Alice from this channel");

    chatState.active = { kind: "channel", id: "other", name: "Other" };
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).not.toContain(
      "Remove Alice from this channel",
    );
    expect(chatState.removeMember).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});

it("does not carry a dissolution confirmation into another channel", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="dissolve-channel-trigger"]',
        )!
        .click(),
    );
    expect(
      container.querySelector('[data-testid="dissolve-channel-confirm"]'),
    ).not.toBeNull();

    chatState.active = { kind: "channel", id: "other", name: "Other" };
    await act(async () => root.render(createElement(MembersDialog)));
    expect(
      container.querySelector('[data-testid="dissolve-channel-confirm"]'),
    ).toBeNull();
    expect(chatState.dissolveChannel).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});

it("does not show the previous channel's activity while the new channel loads", async () => {
  channelApi.channelActivity.mockImplementation((channelId: string) =>
    channelId === "channel"
      ? Promise.resolve([
          {
            kind: "created" as const,
            actor: "owner-device",
            target: null,
            wall_clock: 1,
          },
        ])
      : new Promise(() => {}),
  );
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    expect(container.textContent).toContain("Me created this channel");

    chatState.active = { kind: "channel", id: "other", name: "Other" };
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).not.toContain("Me created this channel");
  } finally {
    await act(async () => root.unmount());
  }
});

it("allows manual dismissal while dissolution remains pending without requesting cancellation", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  let finish!: (success: unknown) => void;
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
    ).toBeNull();
    expect(chatState.cancelGroupOperation).not.toHaveBeenCalled();
    await act(async () =>
      finish({
        kind: "pending",
        id: "background-op",
        status: { phase: { kind: "waiting_for_executor" } },
      }),
    );
    expect(chatState.dissolveChannel).toHaveBeenCalledOnce();
    act(() => dialog.onOpenChange!(false));
    expect(
      container.querySelector('[data-testid="dissolve-channel-trigger"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
  }
});

it("uses the latest durable account name instead of a stale member snapshot offline", () => {
  chatState.members = [
    ...defaultMembers,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Old Alice",
      replica_enabled: true,
    },
  ];
  chatState.peers = [];
  chatState.accounts = [
    { account_id: "peer-account", device_count: 0, names: ["Renamed Alice"] },
  ];
  let html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain("Renamed Alice");
  expect(html).not.toContain("Old Alice");
  expect(html).toContain('src="data:image/png;base64,AAAA"');
  chatState.accounts = [{ ...chatState.accounts[0], names: ["Newest Alice"] }];
  html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain("Newest Alice");
  expect(html).not.toContain("Renamed Alice");
});
it("uses a personal remark in the member list without replacing the canonical account name", () => {
  chatState.members = [
    ...defaultMembers,
    {
      user_id: "peer-device",
      account_id: "peer-account",
      name: "Old Alice",
      replica_enabled: true,
    },
  ];
  chatState.accounts = [
    { account_id: "peer-account", device_count: 0, names: ["Canonical Alice"] },
  ];
  chatState.favorites = { "peer-account": { custom_alias: "My colleague" } };
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain('aria-label="My colleague"');
  expect(chatState.accounts[0].names).toEqual(["Canonical Alice"]);
});

it("shows owner controls and authoritative account badge for a linked owner device", () => {
  chatState.myId = "linked-owner-device";
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      member_count: 1,
      owner: "old-owner-device",
      owner_account_id: "owner-account",
      can_manage: true,
    },
  ];
  chatState.channelOwner = "old-owner-device";
  chatState.members = [
    { user_id: "linked-owner-device", account_id: "owner-account", name: "Me" },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain('data-testid="dissolve-channel-trigger"');
  expect(html).toContain('data-testid="member-owner-badge"');
});
it("does not infer management from account roster identity or matching display names", () => {
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      member_count: 1,
      owner: "owner-device",
      owner_account_id: "owner-account",
      can_manage: false,
    },
  ];
  chatState.members = [
    { user_id: "owner-device", account_id: "owner-account", name: "Me" },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).not.toContain('data-testid="dissolve-channel-trigger"');
});

it("shows a partial reference roster count without membership controls or invented presence", () => {
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      owner: "owner-device",
      member_count: 256,
      shared_reference: true,
      reference_members_complete: false,
      can_manage: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain("256 members");
  expect(html).toContain("Only known members are shown");
  for (const text of [
    "dissolve-channel-trigger",
    "Disable replica",
    "Leave channel",
    "Add member",
    "Updating member presence",
    "known online",
  ])
    expect(html).not.toContain(text);
});

it("retires a pending removal when the same group becomes shared information", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  chatState.members = [
    ...defaultMembers,
    { user_id: "peer-device", account_id: "peer-account", name: "Alice" },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-remove-account-peer-account"]',
        )!
        .click(),
    );
    expect(container.textContent).toContain("Remove Alice");
    chatState.channels = [
      {
        channel_id: "channel",
        name: "Group",
        owner: "owner-device",
        member_count: 2,
        shared_reference: true,
        can_manage: true,
      },
    ];
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).not.toContain("Remove Alice");
    expect(container.textContent).not.toContain("Transfer ownership");
    expect(chatState.removeMemberAccount).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

it("shows a temporary file-sharing pause without presenting the saved replica preference as off", async () => {
  chatState.members = [
    {
      user_id: "owner-device",
      account_id: "owner-account",
      name: "Me",
      replica_enabled: true,
      file_sharing: {
        network_cost: "restricted",
        paused: true,
        override_enabled: false,
      },
    },
  ];
  const { MembersDialog } = await import("./MembersDialog");
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html).toContain("Automatic attachment sharing is paused to save data");
  expect(html).toContain("There is no daily upload cap");
  expect(html).toContain("Allow temporarily");
  expect(html).toContain("Stop sharing");
  expect(html).not.toContain("Replica off");
});

it("keeps unknown cost explicit and sends an owner-bound exception without claiming success after failure", async () => {
  chatState.members = [
    {
      ...defaultMembers[0],
      file_sharing: {
        network_cost: "unknown",
        paused: true,
        override_enabled: false,
      },
    },
  ];
  channelApi.setGroupFileSharingOverride.mockRejectedValueOnce(
    new Error("network changed"),
  );
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).toContain("network cost is unknown");
    const allow = Array.from(container.querySelectorAll("button")).find(
      (button) => button.textContent === "Allow temporarily",
    )!;
    await act(async () => allow.click());
    expect(channelApi.setGroupFileSharingOverride).toHaveBeenCalledWith(
      "local-owner",
      "unknown",
      true,
    );
    expect(container.textContent).toContain("network cost is unknown");
    expect(container.textContent).not.toContain(
      "Automatic attachment sharing is temporarily allowed",
    );
    expect(container.textContent).toContain("Could not update sharing");
    expect(channelApi.setChannelReplicaEnabled).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});

it("lets the local device revoke an exception while manual share-off hides temporary controls", async () => {
  chatState.members = [
    {
      ...defaultMembers[0],
      file_sharing: {
        network_cost: "restricted",
        paused: false,
        override_enabled: true,
      },
    },
  ];
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).toContain(
      "Automatic attachment sharing is temporarily allowed",
    );
    const revoke = Array.from(container.querySelectorAll("button")).find(
      (button) => button.textContent === "Resume data protection",
    )!;
    await act(async () => revoke.click());
    expect(channelApi.setGroupFileSharingOverride).toHaveBeenCalledWith(
      "local-owner",
      "restricted",
      false,
    );
    chatState.members = [{ ...chatState.members[0], replica_enabled: false }];
    await act(async () => root.render(createElement(MembersDialog)));
    expect(container.textContent).not.toContain("Resume data protection");
    expect(container.textContent).toContain("Resume sharing");
  } finally {
    await act(async () => root.unmount());
  }
});

it("labels the owner account on a shared sibling reference even when the original owner device is absent", () => {
  chatState.myId = "linked-owner-device";
  chatState.channelOwner = "old-owner-device";
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      member_count: 2,
      owner: "old-owner-device",
      shared_reference: true,
      account_access: {
        account_id: "owner-account",
        role: "owner",
        execution: { kind: "waiting_for_device" },
      },
    },
  ];
  chatState.members = [
    { user_id: "linked-owner-device", account_id: "owner-account", name: "Me" },
  ];
  const html = renderToStaticMarkup(createElement(MembersDialog));
  expect(html.match(/data-testid="member-owner-badge"/g)).toHaveLength(1);
  expect(html).toContain(
    "Transfer ownership to another member before you leave this channel.",
  );
  expect(html).not.toContain("Membership and sending stay on the device");
  expect(html).not.toContain("Disable replica");
});

it("uses refreshed account ownership when the local signer and source owner device differ", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  chatState.myId = "linked-owner-device";
  chatState.channelOwner = "old-owner-device";
  chatState.members = [
    { user_id: "linked-owner-device", account_id: "owner-account", name: "Me" },
  ];
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      member_count: 2,
      owner: "old-owner-device",
      shared_reference: true,
      account_access: {
        account_id: "owner-account",
        role: "member",
        execution: { kind: "waiting_for_device" },
      },
    },
  ];
  channelApi.channelMembers.mockResolvedValue({
    owner: "old-owner-device",
    owner_account_id: null,
    can_manage: false,
    history_policy: "off",
    members: chatState.members,
    account_access: {
      account_id: "owner-account",
      role: "owner",
      execution: { kind: "waiting_for_device" },
    },
  });
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    expect(
      container.querySelector('[data-testid="member-owner-badge"]'),
    ).toBeNull();
    await act(async () => dialog.onOpenChange?.(true));
    expect(
      container.querySelectorAll('[data-testid="member-owner-badge"]'),
    ).toHaveLength(1);
    expect(
      container.querySelector('[data-testid="dissolve-channel-trigger"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

it("keeps dissolution confirmation open after accepted pending work", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  chatState.dissolveChannel.mockResolvedValueOnce({
    kind: "pending",
    id: "op",
    status: { phase: { kind: "waiting_for_executor" } },
  });
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
    expect(container.textContent).toContain("your linked devices");
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

it("offers an account transfer on a shared owner sibling and retains its pending confirmation", async () => {
  chatState.myId = "linked-owner-device";
  chatState.channelOwner = "source-device";
  chatState.channels = [
    {
      channel_id: "channel",
      name: "Group",
      member_count: 2,
      owner: "source-device",
      shared_reference: true,
      account_access: {
        account_id: "owner-account",
        role: "owner",
        execution: { kind: "waiting_for_device" },
      },
    },
  ];
  chatState.members = [
    { user_id: "linked-owner-device", account_id: "owner-account", name: "Me" },
    { user_id: "peer-device", account_id: "peer-account", name: "Alice" },
  ];
  chatState.transferChannelOwner.mockResolvedValueOnce({
    kind: "pending",
    id: "transfer-op",
    status: { phase: { kind: "waiting_for_executor" } },
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(createElement(MembersDialog)));
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-transfer-account-peer-account"]',
        )!
        .click(),
    );
    await act(async () =>
      [...host.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent === "Transfer ownership")!
        .click(),
    );
    expect(chatState.transferChannelOwner).toHaveBeenCalledExactlyOnceWith(
      "peer-account",
    );
    expect(host.textContent).toContain("Make Alice the channel owner?");
    expect(host.textContent).toContain("your linked devices");
    expect(host.textContent).not.toContain("Disable replica");
  } finally {
    await act(async () => root.unmount());
  }
});
