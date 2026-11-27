import { expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./sensitiveInvoke", () => ({ invokeSensitive: vi.fn() }));
import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";
import { accountTotp } from "./accountTotp";
it("reads status without secrets and routes mutations through native reauthentication", async () => {
  await accountTotp.status("owner");
  expect(invoke).toHaveBeenCalledWith("account_totp_status", {
    owner: "owner",
  });
  await accountTotp.confirm("owner", "123456");
  expect(invokeSensitive).toHaveBeenCalledWith("account_totp_confirm", {
    owner: "owner",
    code: "123456",
  });
});

it("forwards optional login factors without breaking legacy login", async () => {
  const { auth } = await import("./auth");
  await auth.login("alice", "password", "RECOVERY-CODE");
  expect(invoke).toHaveBeenLastCalledWith("login", {
    username: "alice",
    password: "password",
    totp: "RECOVERY-CODE",
  });
  await auth.login("alice", "password");
  expect(invoke).toHaveBeenLastCalledWith("login", {
    username: "alice",
    password: "password",
  });
});
