import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";

const { chatState, policyState } = vi.hoisted(() => ({
  chatState: {
    active: null,
    accounts: [] as Array<{ account_id: string }>,
    channels: [] as Array<{ channel_id: string }>,
    favorites: {},
    peers: [],
    members: [],
    messages: {},
    reactions: {},
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
  captureChatOwnership: () => ({ current: () => true }),
}));
vi.mock("@/store/auth", () => ({
  useAuth: (selector: (state: { user: null }) => unknown) =>
    selector({ user: null }),
}));
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

import { ConversationView } from "./ConversationView";

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
