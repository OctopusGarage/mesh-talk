import { expect, it } from "vitest";
import {
  canManageChannel,
  canParticipateChannel,
  isChannelInvitation,
} from "./channelPermissions";

it("accepts native owner-account rights on another certified device", () => {
  expect(
    canManageChannel(
      { owner: "original-device", can_manage: true },
      "linked-device",
    ),
  ).toBe(true);
});
it("honors native denial even when the legacy device owner matches", () => {
  expect(canManageChannel({ owner: "me", can_manage: false }, "me")).toBe(
    false,
  );
});
it("falls back to exact legacy device ownership only when the new field is absent", () => {
  expect(canManageChannel({ owner: "me" }, "me")).toBe(true);
  expect(canManageChannel({ owner: "original-device" }, "linked-device")).toBe(
    false,
  );
  expect(canManageChannel({ owner: "" }, "")).toBe(false);
  expect(canManageChannel(undefined, "me")).toBe(false);
});

it("denies shared-reference authority even when stale native or owner flags grant it", () => {
  expect(
    canManageChannel(
      { owner: "me", can_manage: true, shared_reference: true },
      "me",
    ),
  ).toBe(false);
});
it.each(["unknown", "member", "invited", "left", "dissolved"] as const)(
  "explicit %s never falls back to matching physical owner",
  (role) => {
    expect(
      canManageChannel(
        {
          owner: "me",
          can_manage: true,
          account_access: {
            account_id: "account",
            role,
            execution: { kind: "local" },
          },
        },
        "me",
      ),
    ).toBe(false);
  },
);
it("waiting eligible signer does not remove account owner role", () => {
  expect(
    canManageChannel(
      {
        owner: "other",
        can_manage: false,
        account_access: {
          account_id: "account",
          role: "owner",
          execution: { kind: "waiting_for_device" },
        },
      },
      "linked",
    ),
  ).toBe(true);
});
it("uses verified account role on a source-waiting shared projection", () => {
  expect(
    canManageChannel(
      {
        owner: "other",
        shared_reference: true,
        account_access: {
          account_id: "account",
          role: "owner",
          execution: { kind: "waiting_for_device" },
        },
      },
      "linked",
    ),
  ).toBe(true);
});
it("previous-account archive cannot grant current owner administration", () => {
  expect(
    canManageChannel(
      {
        owner: "me",
        previous_account: "old",
        account_access: {
          account_id: "old",
          role: "owner",
          execution: { kind: "local" },
        },
      },
      "me",
    ),
  ).toBe(false);
});
it.each(["owner", "member"] as const)(
  "%s can participate with a waiting signer and shared source",
  (role) => {
    expect(
      canParticipateChannel({
        shared_reference: true,
        pending_invite: true,
        account_access: {
          account_id: "me",
          role,
          execution: { kind: "waiting_for_device" },
        },
      }),
    ).toBe(true);
  },
);
it.each(["invited", "left", "unknown", "dissolved"] as const)(
  "%s cannot participate despite old device membership",
  (role) => {
    expect(
      canParticipateChannel({
        account_access: {
          account_id: "me",
          role,
          execution: { kind: "local" },
        },
      }),
    ).toBe(false);
  },
);
it("legacy participation and invitations retain archival boundaries", () => {
  expect(canParticipateChannel({})).toBe(true);
  expect(canParticipateChannel({ shared_reference: true })).toBe(false);
  expect(canParticipateChannel({ left: true })).toBe(false);
  expect(canParticipateChannel({ previous_account: "old" })).toBe(false);
  expect(isChannelInvitation({ pending_invite: true })).toBe(true);
  expect(
    isChannelInvitation({ pending_invite: true, account_access: null }),
  ).toBe(false);
  expect(
    isChannelInvitation({
      previous_account: "old",
      account_access: {
        account_id: "old",
        role: "invited",
        execution: { kind: "local" },
      },
    }),
  ).toBe(false);
});
