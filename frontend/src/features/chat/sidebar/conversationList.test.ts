import { expect, it } from "vitest";
import i18n from "@/lib/i18n";
import type { AccountInfo, ChannelInfo } from "@/lib/types";
import { conversationList, sidebarPeopleCounts } from "./conversationList";

const account = (id: string): AccountInfo => ({
  account_id: id,
  names: [id],
  device_count: 1,
});
const channel = (
  id: string,
  state: Partial<ChannelInfo> = {},
): ChannelInfo => ({
  channel_id: id,
  name: id,
  owner: "me",
  member_count: 3,
  ...state,
});

it("keeps previous-account history outside the current account's group count and pins", () => {
  const current = channel("current");
  const mac = conversationList([], [current], {}, i18n.getFixedT("en"));
  const windows = conversationList(
    [],
    [
      current,
      channel("old", {
        previous_account: "previous",
        left: true,
        pending_invite: true,
      }),
      channel("actual-departure", { left: true }),
    ],
    { old: { pinned: true, custom_alias: null, hidden: false } },
    i18n.getFixedT("en"),
  );
  expect(windows.pinnedRows).toHaveLength(0);
  expect(windows.channelRows.map((row) => row.id)).toEqual([
    "current",
    "actual-departure",
  ]);
  expect(mac.channelRows.map((row) => row.id)).toEqual(["current"]);
  expect(windows.listSize).toBe(2);
  expect(windows.previousAccountRows.map((row) => row.id)).toEqual(["old"]);
  expect(windows.previousAccountRows[0].subtitle).toBe(
    i18n.t("conversation.previousAccountChannel"),
  );
});

it("keeps source order inside sections, aliases and original rename names, excluding hidden channels from navigation", () => {
  const list = conversationList(
    [account("bob"), account("alice"), account("eve")],
    [channel("study"), channel("hidden"), channel("work")],
    {
      alice: { pinned: true, custom_alias: "Ally" },
      hidden: { pinned: true, custom_alias: null, hidden: true },
      study: { pinned: true, custom_alias: "Research" },
    },
    i18n.getFixedT("en"),
  );
  expect(list.pinnedRows.map((r) => [r.id, r.conv.name, r.renameName])).toEqual(
    [
      ["alice", "Ally", "alice"],
      ["study", "Research", "study"],
    ],
  );
  expect(list.accountRows.map((r) => r.id)).toEqual(["bob", "eve"]);
  expect(list.channelRows.map((r) => r.id)).toEqual(["work"]);
  expect(list.hiddenChannels.map((r) => r.id)).toEqual(["hidden"]);
  expect(list.listSize).toBe(5);
});

it("prioritizes dissolved, archived, left and invitation states over message previews", () => {
  const list = conversationList(
    [],
    [
      channel("dissolved", { dissolved: true, archived: true, left: true }),
      channel("archive", { archived: true, left: true }),
      channel("left", { left: true, pending_invite: true }),
      channel("invite", { pending_invite: true }),
      channel("active"),
    ],
    {},
    i18n.getFixedT("en"),
  );
  expect(
    list.channelRows.map((r) => [r.subtitle, r.prioritizeSubtitle]),
  ).toEqual([
    ["Dissolved channel", true],
    ["Archived channel", true],
    ["Left channel", true],
    ["Invitation pending", true],
    ["3 members", false],
  ]);
});

it("deduplicates people across devices, excludes relays and self, and keeps hidden people in network availability only", () => {
  const peer = (user_id: string, account_id: string, post_office = false) => ({
    user_id,
    account_id,
    post_office,
    name: user_id,
    addr: "127.0.0.1",
  });
  const peers = [
    peer("a1", "alice"),
    peer("a2", "alice"),
    peer("b", "bob"),
    peer("r", "relay", true),
    peer("s", "self"),
    peer("o", "offline"),
  ];
  const presence = Object.fromEntries(
    ["alice", "bob", "relay", "self"].map((id) => [
      id,
      { online: true, last_seen_secs: 0 },
    ]),
  );
  expect(
    sidebarPeopleCounts(peers, presence, "self", { bob: true }, true),
  ).toEqual({ networkPeople: 2, onlinePeople: 1 });
  expect(sidebarPeopleCounts(peers, presence, "self", {}, false)).toEqual({
    networkPeople: 2,
    onlinePeople: 0,
  });
});

it.each([false, true])(
  "preserves case-distinct group IDs in pinned=%s lists and prioritizes shared information",
  (pinned) => {
    const list = conversationList(
      [],
      [
        channel("Labali", {
          name: "Labali",
          shared_reference: true,
          archived: true,
          dissolved: true,
          left: true,
          pending_invite: true,
        }),
        channel("labali", { name: "labali" }),
      ],
      pinned
        ? {
            Labali: { pinned: true, custom_alias: null },
            labali: { pinned: true, custom_alias: null },
          }
        : {},
      i18n.getFixedT("en"),
    );
    const rows = pinned ? list.pinnedRows : list.channelRows;
    expect(rows.map((row) => row.id)).toEqual(["Labali", "labali"]);
    expect(rows[0].subtitle).toBe("Shared group information");
    expect(rows[0].prioritizeSubtitle).toBe(true);
    const actual = conversationList(
      [],
      [channel("Labali")],
      {},
      i18n.getFixedT("en"),
    );
    expect(actual.channelRows).toHaveLength(1);
    expect(actual.channelRows[0].subtitle).toBe("3 members");
  },
);
