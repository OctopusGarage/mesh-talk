import { describe, expect, it } from "vitest";
import { visiblePeers, visibleSearchHits } from "./contactVisibility";
const peers = [
  {
    user_id: "old",
    account_id: "hidden",
    name: "Old",
    addr: "a",
    post_office: false,
  },
  {
    user_id: "new",
    account_id: "hidden",
    name: "New",
    addr: "b",
    post_office: false,
  },
  {
    user_id: "visible",
    account_id: "other",
    name: "Visible",
    addr: "c",
    post_office: false,
  },
];
describe("presentation-only visibility projections", () => {
  it("hides all devices regardless of name/address without altering raw peers", () => {
    expect(visiblePeers(peers, { hidden: {} }).map((p) => p.user_id)).toEqual([
      "visible",
    ]);
    expect(peers).toHaveLength(3);
  });
  it("filters account/device-addressed DM hits but retains shared-group hits", () => {
    const hit = (target: string, is_channel = false) => ({
      target,
      is_channel,
      label: "Name",
      text: "text",
      wall_clock: 1,
      from_me: false,
      who: "Name",
    });
    const hits = [
      hit("hidden"),
      hit("new"),
      hit("channel", true),
      hit("other"),
    ];
    expect(
      visibleSearchHits(hits, peers, { hidden: {} }, true).map((h) => h.target),
    ).toEqual(["channel", "other"]);
    expect(
      visibleSearchHits(hits, peers, {}, false).map((h) => h.target),
    ).toEqual(["channel"]);
  });
  it("uses a hit's verified account binding after its device leaves the roster", () => {
    const hit = {
      target: "new",
      account_id: "hidden",
      is_channel: false,
      label: "Name",
      text: "text",
      wall_clock: 1,
      from_me: false,
      who: "Name",
    };
    expect(visibleSearchHits([hit], [], { hidden: {} }, true)).toEqual([]);
  });
});
