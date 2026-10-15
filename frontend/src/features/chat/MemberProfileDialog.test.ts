// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

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
vi.mock("@/store/presence", () => ({
  usePresenceFor: () => ({ online: true, last_seen_secs: 0 }),
  presenceStatus: () => "online",
  presenceLabel: () => "Online",
}));
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
