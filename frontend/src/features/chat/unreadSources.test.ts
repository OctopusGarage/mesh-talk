import { expect, it } from "vitest";
import { unreadSources } from "./unreadSources";
import { unreadTotal } from "@/store/unreadBadge";

it("accounts for every badge source including hidden, orphaned and unknown conversations", () => {
  const state = {
    unread: {
      "account:a": 2,
      "channel:c": 4,
      "account:missing": 1,
      "channel:overflow": 10,
    },
    pendingUnknownUnread: { ghost: 3 },
    accounts: [{ account_id: "a", names: ["Alice"], device_count: 1 }],
    channels: [{ channel_id: "c", name: "Hidden group" }],
    favorites: { c: { hidden: true } },
  };
  const sources = unreadSources(state, {});
  expect(sources.reduce((sum, source) => sum + source.count, 0)).toBe(
    unreadTotal(state),
  );
  expect(sources.find((source) => source.key === "channel:c")).toMatchObject({
    name: "Hidden group",
    hidden: true,
    conversation: { kind: "channel", id: "c" },
  });
  expect(
    sources.find((source) => source.key === "account:missing")?.conversation,
  ).toBeNull();
  expect(
    sources.find((source) => source.key === "pending:ghost"),
  ).toMatchObject({ sender: "ghost", conversation: null });
  expect(
    unreadSources(state, { a: true }).find(
      (source) => source.key === "account:a",
    )?.hidden,
  ).toBe(true);
});

it("can open Message Yourself even when its account is absent from the directory", () => {
  const sources = unreadSources(
    {
      myAccountId: "self",
      unread: { "account:self": 1 },
      pendingUnknownUnread: {},
      accounts: [],
      channels: [],
      favorites: {},
    },
    {},
  );
  expect(sources[0].conversation).toMatchObject({
    kind: "account",
    id: "self",
  });
});
