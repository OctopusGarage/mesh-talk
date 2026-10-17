import { createElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import { setLanguage } from "@/lib/i18n";
import { usePresence } from "@/store/presence";

vi.mock("@/store/presence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/store/presence")>();
  return {
    ...actual,
    usePresenceFor: (id: string) =>
      actual.usePresence.getState().map[id] ?? actual.OFFLINE,
  };
});
vi.mock("@/components/identity", () => ({
  AvatarEditMenu: ({ children }: { children: ReactNode }) => children,
}));
vi.mock("@/components/GroupAvatar", () => ({ GroupAvatar: () => null }));
vi.mock("./MemberProfileDialog", () => ({ MemberProfileDialog: () => null }));
import { ChannelHeader } from "./ConversationHeaders";

beforeEach(() => {
  setLanguage("en");
  usePresence.getState().clear();
});
const render = () =>
  renderToStaticMarkup(
    createElement(ChannelHeader, {
      id: "group",
      name: "Study",
      memberCount: 24,
      archived: false,
      dissolved: false,
      left: false,
      pendingInvite: false,
    }),
  );
it("shows client-relative person counts including zero without a group online dot", () => {
  usePresence.setState({
    map: {
      group: {
        online: false,
        last_seen_secs: null,
        member_count: 24,
        online_count: 0,
        member_keys: [],
      },
    },
  });
  expect(render()).toContain("24 members · 0 known online");
  expect(render()).not.toContain('role="status"');
});
it("localizes person counts and explicitly separates unresolved device membership", () => {
  setLanguage("zh-Hans");
  usePresence.setState({
    map: {
      group: {
        online: false,
        last_seen_secs: null,
        member_count: 24,
        online_count: 6,
        unbound_member_count: 2,
      },
    },
  });
  const html = render();
  expect(html).toContain("24 位成员 · 已知在线 6 人");
  expect(html).toContain("另有 2 个未绑定账号的成员设备");
  setLanguage("en");
});
it("does not label raw device counts as people before the presence snapshot arrives", () => {
  expect(render()).toContain("Updating member presence");
  expect(render()).not.toContain("24 members");
});
