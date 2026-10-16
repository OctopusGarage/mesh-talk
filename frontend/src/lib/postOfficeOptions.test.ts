import { expect, it } from "vitest";
import type { PeerInfo } from "./types";
import {
  selectablePostOffices,
  savedIndirectPostOffice,
} from "./postOfficeOptions";

const peer = (
  user_id: string,
  post_office: boolean,
  discovered_on_lan: boolean,
): PeerInfo => ({
  user_id,
  name: user_id,
  addr: "127.0.0.1:47475",
  post_office,
  discovered_on_lan,
  account_id: null,
});

it("offers only directly discovered LAN relays, regardless of peer order", () => {
  const peers = [
    peer("remote-relay", true, false),
    peer("local-person", false, true),
    peer("windows-relay", true, true),
    peer("mac-relay", true, true),
  ];
  expect(selectablePostOffices(peers).map((p) => p.user_id)).toEqual([
    "mac-relay",
    "windows-relay",
  ]);
});

it("distinguishes a saved remote relay from an offline preference", () => {
  const peers = [peer("remote-relay", true, false)];
  expect(savedIndirectPostOffice(peers, "remote-relay")?.name).toBe(
    "remote-relay",
  );
  expect(savedIndirectPostOffice(peers, "missing")).toBeNull();
  expect(savedIndirectPostOffice(peers, "")).toBeNull();
  expect(
    savedIndirectPostOffice([peer("local", true, true)], "local"),
  ).toBeNull();
});

it("offers direct routed relays without promoting indirect or unknown routes", () => {
  const routed = {
    ...peer("tailscale-relay", true, false),
    directly_reachable: true,
  };
  const indirect = {
    ...peer("gateway-relay", true, false),
    directly_reachable: false,
  };
  const contradictory = {
    ...peer("old-lan-hint", true, true),
    directly_reachable: false,
  };
  expect(
    selectablePostOffices([
      routed,
      indirect,
      contradictory,
      peer("unknown", true, false),
    ]).map((p) => p.user_id),
  ).toEqual(["tailscale-relay"]);
  expect(savedIndirectPostOffice([routed], routed.user_id)).toBeNull();
  expect(savedIndirectPostOffice([indirect], indirect.user_id)?.user_id).toBe(
    indirect.user_id,
  );
});

it("preserves a preference through loss and recovery of its direct route", () => {
  const direct = { ...peer("relay", true, false), directly_reachable: true };
  const indirect = { ...direct, directly_reachable: false };
  expect(selectablePostOffices([indirect])).toEqual([]);
  expect(savedIndirectPostOffice([indirect], "relay")).toBe(indirect);
  expect(selectablePostOffices([direct])).toEqual([direct]);
  expect(savedIndirectPostOffice([direct], "relay")).toBeNull();
  expect(selectablePostOffices([{ ...direct, post_office: false }])).toEqual(
    [],
  );
});
