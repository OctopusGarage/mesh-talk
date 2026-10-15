import { expect, it } from "vitest";
import type { PeerInfo } from "@/lib/types";
import { memberProfileDevices } from "./memberProfileDetails";

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
