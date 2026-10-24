import { expect, it } from "vitest";
import { canManageChannel } from "./channelPermissions";

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
