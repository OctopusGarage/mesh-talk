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
vi.mock("@/store/accountProfiles", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/store/accountProfiles")>();
  return {
    ...actual,
    useAccountProfile: (id: string) =>
      actual.useAccountProfiles.getState().profiles[id],
  };
});
vi.mock("@/components/identity", () => ({
  AvatarEditMenu: ({ children }: { children: ReactNode }) => children,
}));
vi.mock("@/components/GroupAvatar", () => ({ GroupAvatar: () => null }));
vi.mock("./MemberProfileDialog", () => ({ MemberProfileDialog: () => null }));
import { useAccountProfiles } from "@/store/accountProfiles";
import { useAuth } from "@/store/auth";
import { ChannelHeader, ConversationBio } from "./ConversationHeaders";

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
it("labels previous-account history before stale membership flags and presence", () => {
  const html = renderToStaticMarkup(
    createElement(ChannelHeader, {
      id: "group",
      name: "History",
      memberCount: 4,
      archived: true,
      dissolved: false,
      left: true,
      pendingInvite: true,
      previousAccount: true,
    }),
  );
  expect(html).toContain("Previous account");
  expect(html).not.toContain("Left channel");
  expect(html).not.toContain("Archived channel");
  expect(html).not.toContain("Pending invitation");
  expect(html).not.toContain("known online");
});
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

it("shows the active account's About text with a full-text title and never another account's bio", () => {
  useAccountProfiles.setState({
    profiles: {
      alice: {
        account_id: "alice",
        display_name: "Alice",
        bio: "Long personal status 🌱\nSecond line",
        created_at_ms: null,
      },
      bob: {
        account_id: "bob",
        display_name: "Bob",
        bio: "Bob only",
        created_at_ms: null,
      },
    },
  });
  const html = renderToStaticMarkup(
    createElement(ConversationBio, { accountId: "alice" }),
  );
  expect(html).toContain("Long personal status 🌱");
  expect(html).toContain('title="Long personal status');
  expect(html).toContain("truncate");
  expect(html).not.toContain("Bob only");
  const switched = renderToStaticMarkup(
    createElement(ConversationBio, { accountId: "bob" }),
  );
  expect(switched).toContain("Bob only");
  expect(switched).not.toContain("Long personal");
  useAuth.setState({ generation: useAuth.getState().generation + 1 });
  expect(
    renderToStaticMarkup(
      createElement(ConversationBio, { accountId: "alice" }),
    ),
  ).not.toContain("Long personal");
});

it("uses known reference member count without inventing presence or old lifecycle claims", () => {
  const html = renderToStaticMarkup(
    createElement(ChannelHeader, {
      id: "group",
      name: "Study",
      memberCount: 256,
      sharedReference: true,
      archived: true,
      dissolved: true,
      left: true,
      pendingInvite: true,
    }),
  );
  expect(html).toContain("Shared group information");
  expect(html).toContain("256 members");
  expect(html).not.toContain("online");
  expect(html).not.toContain("Updating");
  expect(html).not.toContain("Dissolved");
});

it.each(["waiting_for_device", "requires_upgrade", "reconciling"] as const)(
  "shows compact account execution readiness for %s without calling membership informational",
  (kind) => {
    const html = renderToStaticMarkup(
      createElement(ChannelHeader, {
        id: "group",
        name: "Study",
        memberCount: 2,
        sharedReference: true,
        archived: false,
        dissolved: false,
        left: false,
        pendingInvite: true,
        accountAccess: {
          account_id: "local-account",
          role: "member",
          execution: { kind },
        },
      }),
    );
    expect(html).toContain('data-testid="group-execution-readiness"');
    expect(html).not.toContain("Shared group information");
    expect(html).not.toContain("Pending invitation");
  },
);
