import { expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue({}),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { dmLanguage } from "./language";
it("binds policy reads, writes and validation to an explicit owner", async () => {
  await dmLanguage.get("alice");
  expect(invoke).toHaveBeenLastCalledWith("dm_language_policy", {
    owner: "alice",
    account: null,
  });
  await dmLanguage.get("alice", "peer");
  expect(invoke).toHaveBeenLastCalledWith("dm_language_policy", {
    owner: "alice",
    account: "peer",
  });
  await dmLanguage.set("alice", "ja");
  expect(invoke).toHaveBeenLastCalledWith("set_dm_language_policy", {
    owner: "alice",
    language: "ja",
  });
  await dmLanguage.set("alice", null);
  expect(invoke).toHaveBeenLastCalledWith("set_dm_language_policy", {
    owner: "alice",
    language: null,
  });
  await dmLanguage.validate("alice", "peer", "Hello");
  expect(invoke).toHaveBeenLastCalledWith("validate_dm_language", {
    owner: "alice",
    account: "peer",
    text: "Hello",
  });
});
