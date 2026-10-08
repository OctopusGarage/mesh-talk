import { createElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { chatState, avatars } = vi.hoisted(() => ({
  chatState: {
    members: [
      { user_id: "owner-device", account_id: "owner-account", name: "Me" },
    ],
    channelOwner: "owner-device",
    peers: [
      {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        addr: "127.0.0.1",
        post_office: false,
      },
    ],
    favorites: {},
    myId: "owner-device",
    myAccountId: "owner-account",
    addMember: () => {},
    removeMember: () => {},
    open: () => {},
  },
  avatars: { "peer-account": "data:image/png;base64,AAAA" } as Record<
    string,
    string
  >,
}));

vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
  displayName: (_favorites: unknown, _id: string, name: string) => name,
}));
vi.mock("@/store/auth", () => ({
  useAuth: (selector: (state: unknown) => unknown) =>
    selector({ user: { display_name: "Me" } }),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (selector: (state: unknown) => unknown) =>
    selector({ contacts: {}, loaded: true }),
}));
vi.mock("@/store/presence", () => ({
  usePresence: (selector: (state: unknown) => unknown) => selector({ map: {} }),
  OFFLINE: { online: false, last_seen_secs: 0 },
  presenceStatus: () => "offline",
}));
vi.mock("@/store/avatars", () => ({
  useAvatar: (id: string) => avatars[id],
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
    DialogTrigger: Wrapper,
  };
});

import { MembersDialog } from "./MembersDialog";

it("shows an unjoined peer's account avatar in Add a peer", () => {
  const html = renderToStaticMarkup(createElement(MembersDialog));

  expect(html).toContain("Alice");
  expect(html).toContain('src="data:image/png;base64,AAAA"');
});
