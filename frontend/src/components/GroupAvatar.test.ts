import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";

const { chatState, avatars } = vi.hoisted(() => ({
  chatState: {
    channelMembersById: {
      channel: [
        { user_id: "me", name: "Me", account_id: "my-account" },
        {
          user_id: "offline-device",
          name: "Offline",
          account_id: "offline-account",
        },
      ],
    },
    peers: [],
    myId: "me",
    myAccountId: "my-account",
  },
  avatars: { "offline-account": "data:image/png;base64,AAAA" } as Record<
    string,
    string
  >,
}));
vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: typeof chatState) => unknown) =>
    selector(chatState),
}));
vi.mock("@/store/avatars", () => ({
  useAvatar: (id: string) => avatars[id],
}));
import { GroupAvatar } from "./GroupAvatar";

beforeEach(() => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        { user_id: "me", name: "Me", account_id: "my-account" },
        {
          user_id: "offline-device",
          name: "Offline",
          account_id: "offline-account",
        },
      ],
    },
    peers: [],
  });
});

it("shows a channel member's saved avatar after the member leaves the live roster", () => {
  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );

  expect(html).toContain('src="data:image/png;base64,AAAA"');
});

it("uses the live roster for older member responses without an account id", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [{ user_id: "offline-device", name: "Offline" }],
    },
    peers: [{ user_id: "offline-device", account_id: "offline-account" }],
  });

  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );

  expect(html).toContain('src="data:image/png;base64,AAAA"');
});

it("uses the identity glyph when a member has no known account or avatar", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [{ user_id: "unknown-device", name: "Unknown" }],
    },
  });

  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );

  expect(html).toContain('aria-label="identity glyph"');
  expect(html).not.toContain("data:image/png");
});
