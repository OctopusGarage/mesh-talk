import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";

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
    ready: true,
    bootFailed: false,
  },
  policyState: {
    contacts: {} as Record<string, { hidden: boolean }>,
    loaded: true,
    error: null as "load" | "save" | null,
  },
}));

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
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
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
  chatState.active = null;
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
