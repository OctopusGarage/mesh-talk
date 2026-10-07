import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";

const { chatState } = vi.hoisted(() => ({
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
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
vi.mock("@/lib/platform", () => ({
  needsCustomWindowControls: () => false,
}));
vi.mock("./OfflineConnectDialog", () => ({
  OfflineConnectDialog: () => null,
}));

import { ConversationView } from "./ConversationView";

beforeEach(async () => {
  chatState.active = null;
  chatState.accounts = [];
  chatState.channels = [];
  chatState.ready = true;
  await i18n.changeLanguage("en");
});

it("offers connection guidance when no conversation exists", () => {
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("sidebar.noContacts"));
  expect(html).toContain(i18n.t("conversation.noPeersHint"));
  expect(html).toContain("Connect without Wi-Fi");
});

it("prompts for a selection when a conversation exists", () => {
  chatState.accounts = [{ account_id: "alice" }];
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("conversation.noneSelectedTitle"));
  expect(html).toContain(i18n.t("conversation.noneSelectedDesc"));
  expect(html).not.toContain("Connect without Wi-Fi");
});

it("shows the unlock state while the node is starting", () => {
  chatState.ready = false;
  const html = renderToStaticMarkup(createElement(ConversationView));

  expect(html).toContain(i18n.t("conversation.unlockingTitle"));
  expect(html).not.toContain("Connect without Wi-Fi");
});
