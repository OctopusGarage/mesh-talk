import { expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { accountProfile } from "./api/accountProfile";
it("sends only owner and bio across the profile mutation boundary", async () => {
  await accountProfile.saveBio("owner", "Hello");
  expect(invoke).toHaveBeenCalledWith("save_account_bio", {
    owner: "owner",
    bio: "Hello",
  });
  await accountProfile.get("owner", "account");
  expect(invoke).toHaveBeenCalledWith("get_account_profile", {
    owner: "owner",
    accountId: "account",
  });
});
