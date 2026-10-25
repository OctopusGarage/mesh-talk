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

function tileCount(html: string): number {
  return (html.match(/<img\b|aria-label="identity glyph"/g) ?? []).length;
}

it("renders one tile per account when four devices represent three people", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        { user_id: "me", name: "Me", account_id: "my-account" },
        { user_id: "messi-mac", name: "Messi", account_id: "messi-account" },
        { user_id: "messi-win", name: "Messi", account_id: "messi-account" },
        { user_id: "bob", name: "Bob", account_id: "bob-account" },
      ],
    },
  });
  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );
  expect(tileCount(html)).toBe(3);
});

it("counts only accepted accounts when other devices or accounts have pending invitations", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        { user_id: "me", name: "Me", account_id: "my-account", pending: false },
        {
          user_id: "my-sibling",
          name: "Me",
          account_id: "my-account",
          pending: true,
        },
        {
          user_id: "bob",
          name: "Bob",
          account_id: "bob-account",
          pending: false,
        },
        {
          user_id: "invitee",
          name: "Invited",
          account_id: "invited-account",
          pending: true,
        },
      ],
    },
  });
  expect(
    tileCount(
      renderToStaticMarkup(
        createElement(GroupAvatar, { channelId: "channel" }),
      ),
    ),
  ).toBe(2);
});

it("deduplicates durable offline bindings and the local-account fallback", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        { user_id: "me", name: "Me" },
        { user_id: "my-other-device", name: "Me", account_id: "my-account" },
        {
          user_id: "offline-device",
          name: "Offline",
          account_id: "offline-account",
        },
        {
          user_id: "offline-second",
          name: "Offline",
          account_id: "offline-account",
        },
      ],
    },
    peers: [],
  });
  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );
  expect(tileCount(html)).toBe(2);
  expect((html.match(/src="data:image\/png;base64,AAAA"/g) ?? []).length).toBe(
    1,
  );
});

it("keeps unknown-account devices distinct even when their names match", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        { user_id: "unknown-one", name: "Same name" },
        { user_id: "unknown-two", name: "Same name" },
      ],
    },
  });
  expect(
    tileCount(
      renderToStaticMarkup(
        createElement(GroupAvatar, { channelId: "channel" }),
      ),
    ),
  ).toBe(2);
});

it("applies the nine-account limit after collapsing devices", () => {
  Object.assign(chatState, {
    channelMembersById: {
      channel: [
        ...Array.from({ length: 9 }, (_, index) => ({
          user_id: `same-${index}`,
          name: "Same",
          account_id: "same-account",
        })),
        ...Array.from({ length: 10 }, (_, index) => ({
          user_id: `other-${index}`,
          name: `Other ${index}`,
          account_id: `other-account-${index}`,
        })),
      ],
    },
  });
  const html = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );
  expect(tileCount(html)).toBe(9);
  const paths = [...html.matchAll(/<svg[^>]*>(.*?)<\/svg>/g)].map(
    (match) => match[1],
  );
  expect(new Set(paths).size).toBe(9);
});

it("lets an explicit channel avatar replace the member montage", () => {
  avatars.channel = "data:image/png;base64,CHANNEL";
  try {
    const html = renderToStaticMarkup(
      createElement(GroupAvatar, { channelId: "channel" }),
    );
    expect(html).toContain('src="data:image/png;base64,CHANNEL"');
    expect(html).not.toContain('data-testid="group-avatar"');
    expect(tileCount(html)).toBe(1);
  } finally {
    delete avatars.channel;
  }
});

it("keeps the same account artwork when sibling device order changes", () => {
  const members = [
    { user_id: "messi-win", name: "Messi", account_id: "messi-account" },
    { user_id: "messi-mac", name: "Messi", account_id: "messi-account" },
    { user_id: "bob", name: "Bob", account_id: "bob-account" },
  ];
  Object.assign(chatState, { channelMembersById: { channel: members } });
  const before = renderToStaticMarkup(
    createElement(GroupAvatar, { channelId: "channel" }),
  );
  Object.assign(chatState, {
    channelMembersById: { channel: [members[1], members[0], members[2]] },
  });
  expect(
    renderToStaticMarkup(createElement(GroupAvatar, { channelId: "channel" })),
  ).toBe(before);
});
