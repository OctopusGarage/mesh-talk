// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";

const { chatState, policyState } = vi.hoisted(() => ({
  chatState: {
    active: null as null | { kind: string; id: string; name: string },
    unread: { "account:alice": 2 },
    pendingUnknownUnread: {},
    ledger: {},
    messages: {
      "account:alice": [{ text: "See you on the LAN" }],
    } as Record<string, Array<{ text?: string; file?: { name: string } }>>,
    open: () => {},
    accounts: [{ account_id: "alice", names: ["Alice"], device_count: 1 }],
    channels: [] as import("@/lib/types").ChannelInfo[],
    peers: [],
    favorites: {} as Record<
      string,
      { pinned: boolean; custom_alias: string | null; hidden?: boolean }
    >,
    togglePinned: () => {},
    setChannelHidden: () => {},
    setAlias: () => {},
    renameChannel: vi.fn(async (): Promise<unknown> => ({ kind: "committed" })),
    runEpoch: 1,
    identityEpoch: 1,
    groupOperations: {} as Record<
      string,
      import("@/store/accountGroupOperations").GroupOperationTicket
    >,
    cancelGroupOperation: vi.fn(async () => ({ kind: "pending" })),
    refreshGroupOperations: vi.fn(async () => {}),
    myId: "me",
    myAccountId: "my-account",
    ready: true,
    bootFailed: false,
    retryBoot: () => {},
  },
  policyState: {
    contacts: {} as Record<string, boolean>,
    loaded: true,
    error: null,
  },
}));

// Substitute only virtualization layout: keep the real section and row rendering.
vi.mock("react-virtuoso", () => ({
  Virtuoso: ({
    data,
    itemContent,
  }: {
    data: unknown[];
    itemContent: (index: number, item: unknown) => React.ReactNode;
  }) =>
    createElement(
      "div",
      { "data-testid": "virtual-section" },
      data.map((item, index) => itemContent(index, item)),
    ),
}));

vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
  convKey: (conv: { kind: string; id: string }) => `${conv.kind}:${conv.id}`,
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (selector: (state: typeof policyState) => unknown) =>
    selector(policyState),
}));
vi.mock("@/store/auth", () => {
  const authState = {
    user: { id: "owner", username: "Me" },
    generation: 0,
    logout: () => {},
  };
  return {
    useAuth: Object.assign(
      (selector: (state: typeof authState) => unknown) => selector(authState),
      { getState: () => authState, subscribe: () => () => {} },
    ),
  };
});
vi.mock("@/store/settings", () => ({
  useSettings: (selector: (state: unknown) => unknown) =>
    selector({ callsEnabled: false }),
}));
vi.mock("@/store/presence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/store/presence")>()),
  usePresence: (selector: (state: unknown) => unknown) => selector({ map: {} }),
  usePresenceFor: () => ({ online: false, last_seen_secs: null }),
  presenceStatus: () => "offline",
  presenceLabel: () => "Offline",
  groupPresenceLabel: () => "3 members · 0 known online",
}));
vi.mock("@/lib/theme", () => ({
  useTheme: (selector: (state: unknown) => unknown) =>
    selector({ theme: "dark", toggle: () => {} }),
}));
vi.mock("./HiddenContactsDialog", () => ({
  ContactContextMenu: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock("./CreateChannelDialog", () => ({ CreateChannelDialog: () => null }));
vi.mock("./SearchDialog", () => ({ SearchDialog: () => null }));
vi.mock("./FilesTray", () => ({ FilesTray: () => null }));
vi.mock("./LinkDeviceDialog", () => ({ LinkDeviceDialog: () => null }));
vi.mock("./DiagnosticsDialog", () => ({ DiagnosticsDialog: () => null }));
vi.mock("./OfflineConnectDialog", () => ({ OfflineConnectDialog: () => null }));
vi.mock("./SettingsDialog", () => ({ SettingsDialog: () => null }));
vi.mock("./ProfileDialog", () => ({ ProfileDialog: () => null }));
vi.mock("./AboutDialog", () => ({ AboutDialog: () => null }));
vi.mock("@/components/GroupAvatar", () => ({
  GroupAvatar: ({ title }: { title: string }) =>
    createElement("span", {}, title),
}));

vi.mock("./useSerialPoll", () => ({ useSerialPoll: () => {} }));
vi.mock("./sidebar/SidebarUtilities", () => ({ SidebarUtilities: () => null }));
import { Sidebar } from "./Sidebar";

function expectConversation(
  html: string,
  id: string,
  name: string,
  preview: string,
) {
  const row = html.match(
    new RegExp(`data-testid="conversation-row-${id}"[^>]*>(.*?)</button>`, "s"),
  );
  expect(row).not.toBeNull();
  expect(row?.[1]).toContain(name);
  expect(row?.[1]).toContain(preview);
}

beforeEach(async () => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  chatState.groupOperations = {};
  chatState.renameChannel.mockReset().mockResolvedValue({ kind: "committed" });
  chatState.accounts = [
    { account_id: "alice", names: ["Alice"], device_count: 1 },
  ];
  chatState.channels = [];
  chatState.messages = {
    "account:alice": [{ text: "See you on the LAN" }],
  };
  chatState.active = null;
  chatState.myAccountId = "my-account";
  chatState.ready = true;
  chatState.favorites = {};
  policyState.contacts = {};
  policyState.loaded = true;
  await i18n.changeLanguage("en");
});

it("keeps previous-account history accessible outside the active group list", () => {
  chatState.channels = [
    {
      channel_id: "old",
      name: "Prior group",
      owner: "owner",
      member_count: 2,
      left: true,
      previous_account: "previous",
    },
    {
      channel_id: "current",
      name: "Current group",
      owner: "me",
      member_count: 2,
    },
  ];
  chatState.favorites.old = { pinned: true, custom_alias: null };
  const html = renderToStaticMarkup(createElement(Sidebar));
  expect(html).not.toContain('data-testid="conversation-row-old"');
  expect(html).toContain('data-testid="conversation-row-current"');
  const history = html.match(
    /<details([^>]*data-testid="previous-account-groups"[^>]*)>([\s\S]*?)<\/details>/u,
  );
  expect(history).not.toBeNull();
  expect(history?.[1]).not.toMatch(/\bopen(?:=|\s|$)/u);
  expect(history?.[2]).toContain("<button");
  expect(history?.[2]).toContain("Prior group");
});

it("keeps Message Yourself available without peers or loaded contacts", () => {
  chatState.accounts = [];
  policyState.loaded = false;
  policyState.contacts = { "my-account": true };
  const html = renderToStaticMarkup(createElement(Sidebar));
  expect(html).toContain('data-testid="conversation-row-my-account"');
  expect(html).toContain("Message Yourself");
  expect(html).not.toContain('data-testid="contact-profile-my-account"');
  expect(html).not.toContain('data-testid="conversation-actions-my-account"');
});

it("shows the personal conversation once even when its account is discovered", () => {
  chatState.accounts.push({
    account_id: "my-account",
    names: ["My other device"],
    device_count: 2,
  });
  const html = renderToStaticMarkup(createElement(Sidebar));
  expect(html.match(/data-testid="conversation-row-my-account"/g)).toHaveLength(
    1,
  );
  expect(html).toContain("Message Yourself");
});

it("replaces the personal row with the current runtime account", () => {
  chatState.myAccountId = "adopted-account";
  const html = renderToStaticMarkup(createElement(Sidebar));
  expect(html).toContain('data-testid="conversation-row-adopted-account"');
  expect(html).not.toContain('data-testid="conversation-row-my-account"');
  chatState.accounts.push({
    account_id: "adopted-account",
    names: ["Other device"],
    device_count: 1,
  });
  chatState.ready = false;
  expect(renderToStaticMarkup(createElement(Sidebar))).not.toContain(
    'data-testid="conversation-row-adopted-account"',
  );
});

it("shows a contact's latest message and unread count in the peer list", () => {
  const html = renderToStaticMarkup(createElement(Sidebar));

  expect(html).toContain('data-testid="conversation-row-alice"');
  expect(html).toContain("See you on the LAN");
  expectConversation(html, "alice", "Alice", "See you on the LAN");
  expect(html).toContain(">2</span>");
  expect(html).toContain('data-testid="contact-profile-alice"');
});

it("excludes a hidden contact from the visible peer list", () => {
  policyState.contacts = { alice: true };
  const html = renderToStaticMarkup(createElement(Sidebar));

  expect(html).not.toContain('data-testid="conversation-row-alice"');
  expect(html).not.toContain("See you on the LAN");
});

it("keeps a pinned active contact identifiable by its personal alias", () => {
  chatState.active = { kind: "account", id: "alice", name: "Ally" };
  chatState.favorites = { alice: { pinned: true, custom_alias: "Ally" } };
  const html = renderToStaticMarkup(createElement(Sidebar));

  expect(html).toContain('aria-current="true"');
  expectConversation(html, "alice", "Ally", "See you on the LAN");
  expect(html).toContain('aria-label="More Ally"');
});

it("uses an attachment name as the latest peer preview", () => {
  chatState.messages = {
    "account:alice": [{ file: { name: "project.pdf" } }],
  };
  const html = renderToStaticMarkup(createElement(Sidebar));

  expectConversation(html, "alice", "Alice", "project.pdf");
});

it("falls back to presence and member count when conversations have no messages", () => {
  chatState.messages = {};
  chatState.channels = [
    { channel_id: "group", name: "Study", owner: "me", member_count: 3 },
  ];
  const html = renderToStaticMarkup(createElement(Sidebar));

  expectConversation(html, "alice", "Alice", "Offline");
  expectConversation(html, "group", "Study", "3 members · 0 known online");
});

it("labels a recovered channel as an archive", () => {
  chatState.messages = {};
  chatState.channels = [
    {
      channel_id: "group",
      name: "Study",
      owner: "old-device",
      member_count: 3,
      archived: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(Sidebar));
  expectConversation(html, "group", "Study", "Archived channel");
});

it("keeps a hidden channel in a restore section instead of the active list", () => {
  chatState.channels = [
    { channel_id: "group", name: "Study", owner: "me", member_count: 2 },
  ];
  chatState.favorites = {
    group: { pinned: false, custom_alias: null, hidden: true },
  };
  const html = renderToStaticMarkup(createElement(Sidebar));
  expect(html).not.toContain('data-testid="conversation-row-group"');
  expect(html).toContain("Hidden channels (1)");
  expect(html).toContain("Restore");
});

it("shows a left channel status ahead of its last message", () => {
  chatState.channels = [
    {
      channel_id: "group",
      name: "Study",
      owner: "peer",
      member_count: 2,
      left: true,
    },
  ];
  chatState.messages = { "channel:group": [{ text: "Old message" }] };
  const html = renderToStaticMarkup(createElement(Sidebar));
  expectConversation(html, "group", "Study", "Left channel");
  expect(html).not.toContain("Old message");
});

it.each([80, 81])(
  "preserves account order, preview and navigation positions with %i contacts",
  (count) => {
    chatState.accounts = Array.from({ length: count }, (_, index) => ({
      account_id: `person-${index}`,
      names: [`Person ${index}`],
      device_count: 1,
    }));
    chatState.channels = [
      { channel_id: "group", name: "Study", owner: "me", member_count: 3 },
    ];
    chatState.favorites = {
      "person-0": { pinned: true, custom_alias: "First" },
    };
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).toContain('data-nav-position="1"');
    expect(html).toContain(`data-nav-position="${count + 2}"`);
    expectConversation(html, "person-0", "First", "Offline");
    expectConversation(html, "group", "Study", "3 members · 0 known online");
    expect(html.includes('data-testid="virtual-section"')).toBe(count === 81);
    expect(html.indexOf('conversation-row-person-1"')).toBeLessThan(
      html.indexOf('conversation-row-group"'),
    );
  },
);

it("does not reveal contact rows before the visibility policy has loaded", () => {
  policyState.loaded = false;
  try {
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).not.toContain('data-testid="conversation-row-alice"');
  } finally {
    policyState.loaded = true;
  }
});

it.each(["owner", "member"] as const)(
  "renders the latest message for a verified account %s despite a physical shared reference",
  (role) => {
    chatState.channels = [
      {
        channel_id: "group",
        name: "Study",
        owner: "source-device",
        member_count: 3,
        shared_reference: true,
        archived: true,
        left: true,
        pending_invite: true,
        account_access: {
          account_id: "my-account",
          role,
          execution: { kind: "waiting_for_device" },
        },
      },
    ];
    chatState.messages = {
      "channel:group": [{ text: "Latest group message" }],
    };
    const html = renderToStaticMarkup(createElement(Sidebar));
    expectConversation(html, "group", "Study", "Latest group message");
    expect(html).not.toContain("Shared group information");
  },
);
it("keeps unresolved account access ahead of an old group preview", () => {
  chatState.channels = [
    {
      channel_id: "group",
      name: "Study",
      owner: "source-device",
      member_count: 3,
      account_access: {
        account_id: "my-account",
        role: "unknown",
        execution: { kind: "reconciling" },
      },
    },
  ];
  chatState.messages = { "channel:group": [{ text: "Old group preview" }] };
  const html = renderToStaticMarkup(createElement(Sidebar));
  expectConversation(html, "group", "Study", "Updating group access…");
  expect(html).not.toContain("Old group preview");
});

it("keeps the owner rename dialog open until its original operation commits", async () => {
  chatState.channels = [
    {
      channel_id: "group",
      name: "Study",
      owner: "source",
      member_count: 3,
      shared_reference: true,
      account_access: {
        account_id: "my-account",
        role: "owner",
        execution: { kind: "waiting_for_device" },
      },
    },
  ];
  const status = {
    id: "rename-op",
    account_id: "my-account",
    group: "group",
    kind: "rename" as const,
    phase: { kind: "waiting_for_executor" as const },
    executions: [],
    committed_events: [],
    primary_event: null,
  };
  chatState.renameChannel.mockResolvedValueOnce({
    kind: "pending",
    id: "rename-op",
    status,
  });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () => root.render(createElement(Sidebar)));
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="conversation-actions-group"]',
        )!
        .click(),
    );
    await act(async () =>
      [...document.body.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.trim() === "Rename")!
        .click(),
    );
    const input = document.body.querySelector<HTMLInputElement>(
      'input[placeholder="Channel name"]',
    )!;
    expect(input).not.toBeNull();
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, "Renamed");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () =>
      [...document.body.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent === "Save")!
        .click(),
    );
    expect(chatState.renameChannel).toHaveBeenCalledExactlyOnceWith(
      "group",
      "Renamed",
    );
    expect(document.body.textContent).toContain("your linked devices");
    expect(
      document.body.querySelector('input[placeholder="Channel name"]'),
    ).not.toBeNull();
    chatState.groupOperations["rename-op"] = {
      id: "rename-op",
      account_id: "my-account",
      group: "group",
      kind: "rename",
      stage: "observed",
      observation: 1,
      status: { ...status, phase: { kind: "committed" } },
    };
    await act(async () => root.render(createElement(Sidebar)));
    expect(
      document.body.querySelector('input[placeholder="Channel name"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});

it.each(["my-account", "alice", "group"])(
  "uses a localized image preview for conversation %s",
  async (id) => {
    await i18n.changeLanguage("zh-Hans");
    chatState.channels = [
      { channel_id: "group", name: "Study", owner: "me", member_count: 3 },
    ];
    chatState.messages = {
      [`${id === "group" ? "channel" : "account"}:${id}`]: [
        { file: { name: "screenshot.png" } },
      ],
    };
    const html = renderToStaticMarkup(createElement(Sidebar));
    expectConversation(html, id, "", "[图片]");
    expect(html).not.toContain("screenshot.png");
  },
);
