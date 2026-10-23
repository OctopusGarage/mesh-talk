import { expect, it } from "vitest";
import type { PeerInfo } from "@/lib/types";
import {
  memberDiscoverySources,
  memberProfileDevices,
} from "./memberProfileDetails";

const peers: PeerInfo[] = [
  {
    user_id: "device-a",
    account_id: "account-a",
    name: "Alice",
    addr: "192.168.1.10:47474",
    post_office: false,
  },
  {
    user_id: "device-b",
    account_id: "account-a",
    name: "Alice laptop",
    addr: "100.101.1.2:47474",
    post_office: false,
  },
  {
    user_id: "relay",
    account_id: "relay-account",
    name: "Relay",
    addr: "10.0.0.5:47474",
    post_office: true,
  },
];

it("keeps local discovery and selects one origin per device, deduplicating expired evidence", () => {
  const office = {
    user_id: "office-a",
    name: "mac-mini-post-office",
    endpoint: "10.0.0.1:47479",
  };
  const evidence = [
    { source: "local" as const, last_seen_secs: 0, expires_in_secs: 5 },
    {
      source: "directory" as const,
      last_seen_secs: 0,
      expires_in_secs: 10,
      origin_hops: 0,
      post_office: office,
    },
    {
      source: "directory" as const,
      last_seen_secs: 0,
      expires_in_secs: 10,
      origin_hops: 1,
      post_office: {
        ...office,
        user_id: "office-b",
        name: "mac2015-post-office",
      },
    },
    { source: "routed" as const, last_seen_secs: 0, expires_in_secs: 0 },
  ];
  expect(
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [
        { user_id: "device-a", evidence },
        { user_id: "device-b", evidence },
      ],
    }),
  ).toEqual([{ source: "local" }, { source: "directory", postOffice: office }]);
});

it("leaves the origin unknown when no visible office can be identified", () => {
  const post_office = { endpoint: "192.168.1.4:47479" };
  expect(
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [
        {
          user_id: "device-a",
          evidence: [
            {
              source: "directory",
              last_seen_secs: 0,
              expires_in_secs: 8,
              post_office,
            },
          ],
        },
      ],
    }),
  ).toEqual([{ source: "directory" }]);
  expect(memberDiscoverySources({ online: false, last_seen_secs: 20 })).toEqual(
    [],
  );
});

it("keeps older hosts' directory observations and never labels unscoped discovery as LAN", () => {
  expect(
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [
        {
          user_id: "device-a",
          evidence: [
            { source: "unscoped", last_seen_secs: 0, expires_in_secs: 5 },
            { source: "directory", last_seen_secs: 0, expires_in_secs: 5 },
          ],
        },
      ],
    }),
  ).toEqual([{ source: "unscoped" }, { source: "directory" }]);
});

it("shows every known device of an account with its network endpoint", () => {
  expect(memberProfileDevices({ accountId: "account-a" }, peers)).toEqual([
    { id: "device-a", endpoint: "192.168.1.10:47474", postOffice: false },
    { id: "device-b", endpoint: "100.101.1.2:47474", postOffice: false },
  ]);
});

it("keeps an offline channel member's device ID without inventing an endpoint", () => {
  expect(memberProfileDevices({ deviceId: "old-device" }, peers)).toEqual([
    { id: "old-device", endpoint: null, postOffice: false },
  ]);
});

it("does not mistake a member's own relay role for another peer's network location", () => {
  expect(memberProfileDevices({ accountId: "relay-account" }, peers)).toEqual([
    { id: "relay", endpoint: "10.0.0.5:47474", postOffice: true },
  ]);
});

it("does not attach a changed device identity's endpoint to an older account", () => {
  expect(
    memberProfileDevices({ accountId: "account-a", deviceId: "relay" }, peers),
  ).toEqual([
    { id: "device-a", endpoint: "192.168.1.10:47474", postOffice: false },
    { id: "device-b", endpoint: "100.101.1.2:47474", postOffice: false },
    { id: "relay", endpoint: null, postOffice: false },
  ]);
});

it("selects the closest visible office to the origin, independent of response order", () => {
  const office = (id: string, hops: number, expires = 8) => ({
    source: "directory" as const,
    last_seen_secs: 0,
    expires_in_secs: expires,
    origin_hops: hops,
    post_office: { endpoint: `${id}:47479`, user_id: id, name: id },
  });
  const project = (evidence: ReturnType<typeof office>[]) =>
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [{ user_id: "stella", evidence }],
    });
  const mini = office("mac-mini", 0);
  const mac = office("mac2015", 1);
  const win = office("windows10", 2);
  expect(project([win, mac, mini])).toEqual([
    { source: "directory", postOffice: mini.post_office },
  ]);
  expect(project([win, mac])).toEqual([
    { source: "directory", postOffice: mac.post_office },
  ]);
  expect(project([win, { ...mac, expires_in_secs: 0 }])).toEqual([
    { source: "directory", postOffice: win.post_office },
  ]);
});

it("keeps independent device origins and resolves equal distances stably", () => {
  const entry = (
    id: string,
    hops?: number,
  ): import("@/lib/types").PresenceEvidence => ({
    source: "directory",
    last_seen_secs: 0,
    expires_in_secs: 10,
    origin_hops: hops,
    post_office: { user_id: id, name: id, endpoint: `${id}:1` },
  });
  const project = (evidence: import("@/lib/types").PresenceEvidence[]) =>
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [{ user_id: "a", evidence }],
    });
  expect(project([entry("z", 1), entry("a", 1)])[0].postOffice?.user_id).toBe(
    "a",
  );
  expect(project([entry("a", 1), entry("z", 1)])[0].postOffice?.user_id).toBe(
    "a",
  );
  expect(project([entry("unknown"), entry("bad", -1)])).toEqual([
    { source: "directory" },
  ]);
  expect(
    project([entry("unknown"), entry("known", 2)])[0].postOffice?.user_id,
  ).toBe("known");
  expect(
    memberDiscoverySources({
      online: true,
      last_seen_secs: 0,
      devices: [
        { user_id: "a", evidence: [entry("mini", 0)] },
        { user_id: "b", evidence: [entry("windows", 0)] },
      ],
    }).map((source) => source.postOffice?.user_id),
  ).toEqual(["mini", "windows"]);
});
