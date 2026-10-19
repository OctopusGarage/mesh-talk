// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { setLanguage } from "@/lib/i18n";
import type { PresenceInfo } from "@/lib/types";

const { peers } = vi.hoisted(() => ({
  peers: [
    {
      user_id: "device-a",
      account_id: "account-a",
      name: "Alice",
      addr: "100.101.1.2:47474",
      post_office: false,
    },
  ],
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: { peers: typeof peers }) => unknown) =>
    select({ peers }),
}));
const presenceState = vi.hoisted(() => ({
  value: { online: true, last_seen_secs: 0 } as PresenceInfo,
}));
vi.mock("@/store/presence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/store/presence")>()),
  usePresenceFor: () => presenceState.value,
}));
afterEach(() => {
  presenceState.value = { online: true, last_seen_secs: 0 };
  setLanguage("en");
});
vi.mock("@/components/identity", () => ({
  IdentityCrest: ({ name }: { name: string }) =>
    createElement("span", null, name),
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrapper = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: Wrapper,
    DialogContent: Wrapper,
    DialogHeader: Wrapper,
    DialogTitle: Wrapper,
    DialogDescription: Wrapper,
  };
});

import { MemberProfileDialog } from "./MemberProfileDialog";

it("shows full account and device IDs plus the known network endpoint", () => {
  const html = renderToStaticMarkup(
    createElement(MemberProfileDialog, {
      target: { name: "Alice", accountId: "account-a" },
      onOpenChange: () => {},
    }),
  );
  expect(html).toContain("account-a");
  expect(html).toContain("device-a");
  expect(html).toContain("100.101.1.2:47474");
  expect(html).toContain("Online");
  expect(html).toContain("relay");
});

it("shows an offline member without claiming a network location", () => {
  const html = renderToStaticMarkup(
    createElement(MemberProfileDialog, {
      target: { name: "Former member", deviceId: "old-device" },
      onOpenChange: () => {},
    }),
  );
  expect(html).toContain("old-device");
  expect(html).toContain("Not currently available");
  expect(html).not.toContain("100.101.1.2:47474");
});

it("keeps messaging available for members with a known account", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  const onMessage = vi.fn();
  const onOpenChange = vi.fn();
  try {
    await act(async () =>
      root.render(
        createElement(MemberProfileDialog, {
          target: { name: "Alice", accountId: "account-a" },
          onOpenChange,
          onMessage,
        }),
      ),
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[data-testid="member-profile-message"]',
        )!
        .click(),
    );
    expect(onMessage).toHaveBeenCalledWith("account-a", "Alice");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  } finally {
    await act(async () => root.unmount());
  }
});

it("renders one freshly observed device with overlapping localized sources and offline explanation", () => {
  setLanguage("zh-Hans");
  presenceState.value = {
    online: true,
    last_seen_secs: 0,
    devices: [
      {
        user_id: "device-a",
        evidence: [
          { source: "local", last_seen_secs: 2, expires_in_secs: 28 },
          { source: "routed", last_seen_secs: 1, expires_in_secs: 119 },
          { source: "directory", last_seen_secs: 3, expires_in_secs: 42 },
          { source: "unscoped", last_seen_secs: 0, expires_in_secs: 29 },
        ],
      },
    ],
  };
  const html = renderToStaticMarkup(
    createElement(MemberProfileDialog, {
      target: { name: "Alice", accountId: "account-a" },
      onOpenChange: () => {},
    }),
  );
  expect(html).toContain("发现 1 台在线设备");
  for (const label of [
    "在线",
    "本地网络发现",
    "跨网络发现",
    "经邮局发现",
    "签名发现记录（网络范围未确定）",
    "本机 2 秒前发现",
    "离线表示当前客户端未发现对方的有效在线信息",
  ])
    expect(html).toContain(label);
  expect(html).not.toContain("已连接");
});
it("renders Offline as the primary state with local observation history", () => {
  setLanguage("zh-Hans");
  presenceState.value = { online: false, last_seen_secs: 180, devices: [] };
  const html = renderToStaticMarkup(
    createElement(MemberProfileDialog, {
      target: { name: "Alice", accountId: "account-a" },
      onOpenChange: () => {},
    }),
  );
  expect(html).toContain("离线 · 本机 3 分钟前发现");
  presenceState.value = { online: false, last_seen_secs: null, devices: [] };
  const neverSeen = renderToStaticMarkup(
    createElement(MemberProfileDialog, {
      target: { name: "Alice", deviceId: "legacy" },
      onOpenChange: () => {},
    }),
  );
  expect(neverSeen).toContain("离线");
  expect(neverSeen).not.toContain("状态未知");
  expect(neverSeen).not.toContain("分钟前发现");
});
