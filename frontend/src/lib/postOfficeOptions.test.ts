import { expect, it } from "vitest";
import type { PeerInfo } from "./types";
import { localPostOffices, savedRemotePostOffice } from "./postOfficeOptions";

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
  expect(localPostOffices(peers).map((p) => p.user_id)).toEqual([
    "mac-relay",
    "windows-relay",
  ]);
});

it("distinguishes a saved remote relay from an offline preference", () => {
  const peers = [peer("remote-relay", true, false)];
  expect(savedRemotePostOffice(peers, "remote-relay")?.name).toBe(
    "remote-relay",
  );
  expect(savedRemotePostOffice(peers, "missing")).toBeNull();
  expect(savedRemotePostOffice(peers, "")).toBeNull();
  expect(
    savedRemotePostOffice([peer("local", true, true)], "local"),
  ).toBeNull();
});
