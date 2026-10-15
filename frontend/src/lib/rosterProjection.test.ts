import { expect, it } from "vitest";
import type { PeerInfo } from "./types";
import { rosterProjection } from "./rosterProjection";

it("keeps relay devices available for diagnostics while selecting a human contact", () => {
  const peers: PeerInfo[] = [
    {
      user_id: "relay",
      account_id: "shared",
      name: "Relay",
      addr: "relay:1",
      post_office: true,
    },
    {
      user_id: "human",
      account_id: "shared",
      name: "Alice",
      addr: "human:1",
      post_office: false,
    },
    {
      user_id: "relay-only",
      account_id: "infrastructure",
      name: "Post office",
      addr: "relay:2",
      post_office: true,
    },
    {
      user_id: "legacy",
      account_id: null,
      name: "Legacy device",
      addr: "legacy:1",
      post_office: false,
    },
  ];
  const roster = rosterProjection(peers);

  expect(roster.peerForDevice("relay")?.name).toBe("Relay");
  expect(
    roster.devicesForAccount("shared").map((peer) => peer.user_id),
  ).toEqual(["relay", "human"]);
  expect(roster.contactForAccount("shared")?.user_id).toBe("human");
  expect(roster.contactForAccount("infrastructure")).toBeUndefined();
  expect(roster.contactForAccount("legacy")).toBeUndefined();
  expect(rosterProjection(peers)).toBe(roster);
});

it("prefers a channel member's durable account binding over the live roster", () => {
  const peers: PeerInfo[] = [
    {
      user_id: "device",
      account_id: "new-account",
      name: "New name",
      addr: "peer:1",
      post_office: false,
    },
  ];
  const roster = rosterProjection(peers);
  expect(
    roster.accountForMember({
      user_id: "device",
      account_id: "signed-account",
    }),
  ).toBe("signed-account");
  expect(roster.accountForMember({ user_id: "device" })).toBe("new-account");
  expect(
    roster.accountForMember({
      user_id: "offline",
      account_id: "offline-account",
    }),
  ).toBe("offline-account");
  expect(roster.accountForDevice("offline")).toBeNull();
  expect(roster.nameForDevice("device")).toBe("New name");
});
