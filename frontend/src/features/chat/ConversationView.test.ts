import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useDrafts } from "@/store/drafts";

const { chatState, policyState } = vi.hoisted(() => ({
  chatState: {
    active: null as import("@/store/chat").Conversation | null,
    accounts: [] as Array<{ account_id: string }>,
    channels: [] as import("@/lib/types").ChannelInfo[],
    favorites: {},
    peers: [],
    members: [],
    messages: {},
    reactions: {},
    groupOperations: {} as Record<
      string,
      import("@/store/accountGroupOperations").GroupOperationTicket
    >,
    deleted: {},
    searchTarget: null,
    expiryNow: Date.now(),
    unreadIds: {},
    lateUnreadIds: {},
    overflowUnread: {},
    reception: {},
    markMessagesViewed: vi.fn(),
    runEpoch: 0,
    identityEpoch: 0,
    myId: "local-device",
    myAccountId: "local-account",
    ready: true,
    bootFailed: false,
  },
  policyState: {
    contacts: {} as Record<string, { hidden: boolean }>,
    loaded: true,
    error: null as "load" | "save" | null,
  },
}));

vi.mock("@/store/drafts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/store/drafts")>();
  return {
    ...actual,
    useDrafts: Object.assign(
      (
        selector: (
          state: ReturnType<typeof actual.useDrafts.getState>,
        ) => unknown,
      ) => selector(actual.useDrafts.getState()),
      actual.useDrafts,
    ),
  };
});
vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
  convKey: (conv: { kind: string; id: string }) => `${conv.kind}:${conv.id}`,
  displayName: (_favorites: unknown, _id: string, name: string) => name,
  captureChatOwnership: () => ({ current: () => true }),
}));
vi.mock("@/store/auth", () => {
  const authState = { user: null, generation: 0 };
  return {
    useAuth: Object.assign(
      (selector: (state: typeof authState) => unknown) => selector(authState),
      { getState: () => authState, subscribe: () => () => {} },
    ),
  };
});
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (selector: (state: typeof policyState) => unknown) =>
    selector(policyState),
}));
vi.mock("@/lib/motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/motion")>()),
  useMotionOK: () => false,
}));
vi.mock("@/lib/platform", () => ({
  needsCustomWindowControls: () => false,
}));
vi.mock("./DiagnosticsDialog", () => ({
  DiagnosticsDialog: () => null,
}));
vi.mock("./HiddenContactsDialog", () => ({
  HiddenContactsDialog: () => createElement("button", null, "Manage"),
}));
vi.mock("./PendingChannelInvite", () => ({
  PendingChannelInvite: () =>
    createElement("div", { "data-testid": "pending-invite" }),
}));
vi.mock("./ConversationHeaders", () => ({
  ChannelHeader: () => createElement("header", null, "Group"),
  ConversationBio: () => null,
  DmHeader: () => null,
  PersonalHeader: () => null,
}));

import { ConversationView } from "./ConversationView";

it("explains previous-account history without offering its pending invitation", () => {
  chatState.active = { kind: "channel", id: "old", name: "Prior group" };
  chatState.channels = [
    {
      channel_id: "old",
      name: "Prior group",
      owner: "owner",
      member_count: 2,
      previous_account: "previous",
      pending_invite: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(ConversationView));
  expect(html).not.toContain('data-testid="pending-invite"');
  expect(html).toContain(i18n.t("conversation.previousAccountChannelHint"));
  expect(html).toContain(i18n.t("conversation.previousAccountEmpty"));
  expect(html).not.toContain(i18n.t("redesign.emptyConversation"));
});

it("does not count a previous account's history as a current conversation", () => {
  chatState.channels = [
    {
      channel_id: "old",
      name: "Prior group",
      owner: "owner",
      member_count: 2,
      previous_account: "previous",
      left: true,
    },
  ];
  const html = renderToStaticMarkup(createElement(ConversationView));
  expect(html).toContain(i18n.t("redesign.noPeersTitle"));
  expect(html).not.toContain(i18n.t("conversation.noneSelectedTitle"));
});

it.each(["left", "archived", "dissolved"] as const)(
  "does not offer invitation actions for an unavailable %s group",
  (state) => {
    chatState.active = {
      kind: "channel",
      id: "previous",
      name: "Historical group",
    };
    chatState.channels = [
      {
        channel_id: "previous",
        name: "Historical group",
        owner: "owner",
        member_count: 2,
        pending_invite: true,
        [state]: true,
      },
    ];
    const html = renderToStaticMarkup(createElement(ConversationView));
    expect(html).not.toContain('data-testid="pending-invite"');
    expect(html).toContain('role="status"');
  },
);

it("retains invitation actions for a current pending group", () => {
  chatState.active = {
    kind: "channel",
    id: "invited",
    name: "Current invitation",
  };
  chatState.channels = [
    {
      channel_id: "invited",
      name: "Current invitation",
      owner: "owner",
      member_count: 2,
      pending_invite: true,
    },
  ];
  expect(renderToStaticMarkup(createElement(ConversationView))).toContain(
    'data-testid="pending-invite"',
  );
});

beforeEach(async () => {
  useDrafts.setState({
    owner: null,
    generation: 0,
    loaded: true,
    texts: {},
    protections: {},
    pending: {},
  });
  chatState.active = null;
  chatState.groupOperations = {};
  chatState.accounts = [];
  chatState.channels = [];
  chatState.ready = true;
  chatState.bootFailed = false;
  policyState.contacts = {};
  policyState.loaded = true;
  policyState.error = null;
  await i18n.changeLanguage("en");
});

it("offers connection guidance when no conversation exists", () => {
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("redesign.noPeersTitle"));
  expect(html).toContain(i18n.t("redesign.noPeersBody"));
  expect(html).toContain(i18n.t("redesign.connectionHelp"));
  expect(html).toContain(i18n.t("redesign.connectToSomeone"));
});

it("prompts for a selection when a conversation exists", () => {
  chatState.accounts = [{ account_id: "alice" }];
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("conversation.noneSelectedTitle"));
  expect(html).toContain(i18n.t("conversation.noneSelectedDesc"));
  expect(html).not.toContain(i18n.t("redesign.connectionHelp"));
});

it("offers restoration when every contact is hidden", () => {
  chatState.accounts = [{ account_id: "alice" }];
  policyState.contacts = { alice: { hidden: true } };
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("contactVisibility.allHidden"));
  expect(html).toContain("Manage");
  expect(html).not.toContain(i18n.t("redesign.noPeersTitle"));
});

it("shows contact-policy failure before claiming no peers", () => {
  policyState.loaded = false;
  policyState.error = "load";
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("contactVisibility.loadError"));
  expect(html).not.toContain(i18n.t("redesign.noPeersTitle"));
});

it("shows the unlock state while the node is starting", () => {
  chatState.ready = false;
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("conversation.unlockingTitle"));
  expect(html).not.toContain(i18n.t("redesign.connectionHelp"));
});

it("replaces the unlock spinner with recovery when startup fails", () => {
  chatState.ready = false;
  chatState.bootFailed = true;
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain("Couldn’t start Mesh-Talk");
  expect(html).toContain(i18n.t("diagnostics.revealLogs"));
  expect(html).not.toContain(i18n.t("conversation.unlockingTitle"));
});

it.each(["owner", "member"] as const)(
  "renders the real composer for a shared account %s while its signer is unavailable",
  (role) => {
    chatState.active = { kind: "channel", id: "shared", name: "Study" };
    chatState.channels = [
      {
        channel_id: "shared",
        name: "Study",
        owner: "source-device",
        member_count: 2,
        shared_reference: true,
        pending_invite: true,
        left: true,
        archived: true,
        account_access: {
          account_id: "local-account",
          role,
          execution: { kind: "waiting_for_device" },
        },
      },
    ];
    const html = renderToStaticMarkup(createElement(ConversationView));
    expect(html).toContain("<textarea");
    expect(html).not.toContain('data-testid="pending-invite"');
    expect(html).not.toContain(i18n.t("conversation.sharedReferenceHint"));
  },
);
it("renders invitation actions from the account role despite an absent physical invitation", () => {
  chatState.active = { kind: "channel", id: "shared", name: "Study" };
  chatState.channels = [
    {
      channel_id: "shared",
      name: "Study",
      owner: "source-device",
      member_count: 2,
      shared_reference: true,
      pending_invite: false,
      account_access: {
        account_id: "local-account",
        role: "invited",
        execution: { kind: "via_device", device_id: "source-device" },
      },
    },
  ];
  const html = renderToStaticMarkup(createElement(ConversationView));
  expect(html).toContain('data-testid="pending-invite"');
  expect(html).not.toContain("<textarea");
});
it.each(["left", "dissolved", "unknown"] as const)(
  "does not render a composer when the account role is %s",
  (role) => {
    chatState.active = { kind: "channel", id: "shared", name: "Study" };
    chatState.channels = [
      {
        channel_id: "shared",
        name: "Study",
        owner: "source-device",
        member_count: 2,
        account_access: {
          account_id: "local-account",
          role,
          execution: { kind: "reconciling" },
        },
      },
    ];
    const html = renderToStaticMarkup(createElement(ConversationView));
    expect(html).not.toContain("<textarea");
    expect(html).toContain('role="status"');
  },
);

it("shows a reference-only member's pending message control in its conversation", () => {
  chatState.active = { kind: "channel", id: "group", name: "Group" };
  chatState.channels = [
    {
      channel_id: "group",
      name: "Group",
      owner: "sibling",
      member_count: 2,
      shared_reference: true,
      account_access: {
        account_id: "local-account",
        role: "member",
        execution: { kind: "waiting_for_device" },
      },
    },
  ];
  chatState.groupOperations = {
    original: {
      id: "original",
      account_id: "local-account",
      group: "group",
      kind: "recall",
      stage: "observed",
      observation: 1,
      status: {
        id: "original",
        account_id: "local-account",
        group: "group",
        kind: "recall",
        phase: { kind: "waiting_for_executor" },
        executions: [],
        committed_events: [],
        primary_event: null,
      },
    },
  };
  const html = renderToStaticMarkup(createElement(ConversationView));
  expect(html).toContain("Recall message");
  expect(html).toContain("Waiting for one of your linked devices");
  expect(html).not.toContain("The account action completed");
});
