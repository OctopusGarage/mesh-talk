import { expect, it } from "vitest";
import { displayIdentity } from "./displayIdentity";
const accounts = [
  {
    account_id: "account-alice",
    device_count: 0,
    names: ["Latest Alice", "Old Alice"],
  },
];
it("uses remarks before durable account names and device or message snapshots", () => {
  expect(
    displayIdentity({
      accountId: "account-alice",
      deviceId: "device-alice",
      fallbackName: "Old message",
      accounts,
      favorites: { "account-alice": { custom_alias: "My friend" } },
    }).name,
  ).toBe("My friend");
});
it("retains the latest account name and account avatar key with an empty offline roster", () => {
  expect(
    displayIdentity({ accountId: "account-alice", accounts, peers: [] }),
  ).toMatchObject({ name: "Latest Alice", id: "account-alice" });
});
it("follows canonical roster updates instead of a captured name", () => {
  const input = {
    accountId: "account-alice",
    fallbackName: "Captured Alice",
    accounts,
  };
  expect(displayIdentity(input).name).toBe("Latest Alice");
  expect(
    displayIdentity({
      ...input,
      accounts: [{ ...accounts[0], names: ["Renamed Alice"] }],
    }).name,
  ).toBe("Renamed Alice");
});
it("resolves device IDs only through a verified roster binding", () => {
  const peer = {
    user_id: "device-alice",
    account_id: "account-alice",
    name: "Alice",
    addr: "127.0.0.1",
    post_office: false,
  };
  expect(
    displayIdentity({ deviceId: peer.user_id, peers: [peer], accounts })
      .accountId,
  ).toBe("account-alice");
  expect(
    displayIdentity({ deviceId: peer.user_id, peers: [], accounts }).accountId,
  ).toBeNull();
});
it("uses a message name before a shortened ID when no canonical name is known", () => {
  expect(
    displayIdentity({
      deviceId: "0123456789abcdef",
      fallbackName: "From message",
    }).name,
  ).toBe("From message");
  expect(
    displayIdentity({
      deviceId: "0123456789abcdef",
      fallbackName: "0123456789abcdef",
    }).name,
  ).toBe("01234567");
});

it("keeps canonical profile data separate from a personal remark", () => {
  const identity = displayIdentity({
    accountId: "account-alice",
    accounts,
    favorites: { "account-alice": { custom_alias: "My friend" } },
  });
  expect(identity).toMatchObject({
    name: "My friend",
    alias: "My friend",
    canonicalName: "Latest Alice",
  });
  expect(accounts[0].names[0]).toBe("Latest Alice");
});
it("does not borrow another account's device name over a signed member binding", () => {
  const identity = displayIdentity({
    accountId: "signed-account",
    deviceId: "rebound-device",
    fallbackName: "Signed member",
    peers: [
      {
        user_id: "rebound-device",
        account_id: "another-account",
        name: "Other person",
        addr: "127.0.0.1",
        post_office: false,
      },
    ],
  });
  expect(identity.accountId).toBe("signed-account");
  expect(identity.name).toBe("Signed member");
});
