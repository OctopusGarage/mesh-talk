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

it("keeps local discovery and distinct introducing offices, deduplicating devices and expired evidence", () => {
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
      post_office: office,
    },
    {
      source: "directory" as const,
      last_seen_secs: 0,
      expires_in_secs: 10,
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
  ).toEqual([
    { source: "local" },
    { source: "directory", postOffice: office },
    {
      source: "directory",
      postOffice: {
        ...office,
        user_id: "office-b",
        name: "mac2015-post-office",
      },
    },
  ]);
});

it("preserves unnamed introducing endpoints without borrowing another peer's name", () => {
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
  ).toEqual([{ source: "directory", postOffice: post_office }]);
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
