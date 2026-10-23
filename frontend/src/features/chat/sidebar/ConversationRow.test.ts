// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useChat } from "@/store/chat";
import { emptyReception, viewIdentities } from "@/store/reception";
import { ConversationRow } from "./ConversationRow";

vi.mock("@/components/GroupAvatar", () => ({ GroupAvatar: () => null }));
let host: HTMLDivElement;
let root: Root;
const key = "channel:team";
beforeEach(async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  await i18n.changeLanguage("en");
  useChat.setState({
    ...emptyReception({
      [key]: {
        mention: { seen: false, late: false, mentioned: true },
        ordinary: { seen: false, late: false },
      },
    }),
    active: null,
    unread: { [key]: 2 },
    messages: {},
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function render() {
  await act(async () =>
    root.render(
      createElement(ConversationRow, {
        conv: { kind: "channel", id: "team", name: "Team" },
        subtitle: "Latest ordinary message",
        prioritizeSubtitle: true,
        channel: true,
        pinned: false,
        onTogglePin: () => {},
        onRename: () => {},
        listPosition: 1,
        listSize: 1,
      }),
    ),
  );
}
it("shows a mention prefix beside the latest preview and keeps the unread badge", async () => {
  await render();
  const row = host.querySelector('[data-testid="conversation-row-team"]');
  expect(row?.textContent).toContain("[Mentioned you]");
  expect(row?.textContent).toContain("Latest ordinary message");
  expect(host.textContent).toContain("2");
  await act(async () =>
    useChat.setState((s) => viewIdentities(s, key, ["ordinary"])),
  );
  expect(row?.textContent).toContain("[Mentioned you]");
  await act(async () =>
    useChat.setState((s) => viewIdentities(s, key, ["mention"])),
  );
  expect(row?.textContent).not.toContain("[Mentioned you]");
});
it("keeps the prefix in Chinese with no cached messages", async () => {
  await i18n.changeLanguage("zh-Hans");
  await render();
  expect(host.textContent).toContain("[有人@我]");
});
