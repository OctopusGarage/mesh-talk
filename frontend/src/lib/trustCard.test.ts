import { expect, it, vi } from "vitest";
import {
  creationDateMs,
  checkedTrustArtifact,
  trustDateInput,
  trustDateLabel,
} from "./trustCard";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { trustCards } from "./api/trustCards";
it("keeps unknown creation dates unknown and rejects invalid dates", () => {
  expect(creationDateMs("")).toBeNull();
  expect(creationDateMs("2026-01-01")).toBe(Date.UTC(2026, 0, 1));
  expect(() => creationDateMs("2026-02-30")).toThrow();
});
it("bounds imported JSON before invoking the native parser", () => {
  expect(checkedTrustArtifact(' {"card":{}} ')).toBe('{"card":{}}');
  expect(() => checkedTrustArtifact("x".repeat(1_100_000))).toThrow();
  expect(() => checkedTrustArtifact("[]")).toThrow();
  expect(() => checkedTrustArtifact("invalid")).toThrow();
});
it("binds signed card operations to their owner with native argument names", async () => {
  await trustCards.publish("owner", "Alice", "About Alice", null);
  expect(invoke).toHaveBeenCalledWith("publish_trust_card", {
    owner: "owner",
    displayName: "Alice",
    bio: "About Alice",
    createdAtMs: null,
  });
  await trustCards.introduce("owner", "bob", "Met in person");
  expect(invoke).toHaveBeenCalledWith("create_trust_introduction", {
    owner: "owner",
    accountId: "bob",
    note: "Met in person",
  });
  await trustCards.importArtifact("owner", '{"card":{}}');
  expect(invoke).toHaveBeenCalledWith("import_trust_artifact", {
    owner: "owner",
    json: '{"card":{}}',
  });
});

it("formats untrusted timestamps without throwing or rounding the stored value", () => {
  expect(trustDateInput(Number.MAX_VALUE)).toBe("");
  expect(trustDateLabel(Number.MAX_VALUE)).toBe(
    "outside the supported date range",
  );
  expect(trustDateInput(Date.UTC(2026, 0, 1, 12, 34, 56))).toBe("2026-01-01");
  expect(
    checkedTrustArtifact(
      JSON.stringify({ avatar: Array(128 * 1024).fill(255) }),
    ).length,
  ).toBeGreaterThan(256 * 1024);
});
