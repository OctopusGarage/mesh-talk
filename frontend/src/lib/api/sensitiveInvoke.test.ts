import { afterEach, beforeEach, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  invokeSensitive,
  registerSensitiveVerification,
} from "./sensitiveInvoke";

const required = {
  kind: "authentication-required",
  message: "Verify identity",
};
let unregister: (() => void) | undefined;
beforeEach(() => invoke.mockReset());
afterEach(() => unregister?.());

it("runs ordinary changes without requesting a password", async () => {
  const prompt = vi.fn();
  unregister = registerSensitiveVerification(prompt);
  invoke.mockResolvedValueOnce("saved");
  expect(
    await invokeSensitive("set_app_settings", {
      patch: { notifications: false },
    }),
  ).toBe("saved");
  expect(prompt).not.toHaveBeenCalled();
});

it("retries only the captured operation after backend requests fresh authentication", async () => {
  const args = { destinations: [{ kind: "channel", id: "one" }] };
  const prompt = vi.fn(async () => {
    args.destinations[0].id = "changed";
    return { password: "secret", current: () => true };
  });
  unregister = registerSensitiveVerification(prompt);
  invoke.mockRejectedValueOnce(required).mockResolvedValueOnce("accepted");
  expect(await invokeSensitive("set_local_agent_access", args)).toBe(
    "accepted",
  );
  expect(invoke).toHaveBeenLastCalledWith("set_local_agent_access", {
    destinations: [{ kind: "channel", id: "one" }],
    password: "secret",
  });
  expect(prompt).toHaveBeenCalledWith("set_local_agent_access");
});

it("cancels without issuing a second native command", async () => {
  unregister = registerSensitiveVerification(async () => null);
  invoke.mockRejectedValueOnce(required);
  await expect(invokeSensitive("create_backup", {})).rejects.toMatchObject({
    kind: "authentication-canceled",
  });
  expect(invoke).toHaveBeenCalledTimes(1);
});

it("rejects a replaced session before resubmitting a password", async () => {
  unregister = registerSensitiveVerification(async () => ({
    password: "secret",
    current: () => false,
  }));
  invoke.mockRejectedValueOnce(required);
  await expect(invokeSensitive("start_linking", {})).rejects.toMatchObject({
    kind: "authentication-canceled",
  });
  expect(invoke).toHaveBeenCalledTimes(1);
});

it("does not retry invalid credentials or unrelated failures", async () => {
  const prompt = vi.fn(async () => ({
    password: "wrong",
    current: () => true,
  }));
  unregister = registerSensitiveVerification(prompt);
  invoke
    .mockRejectedValueOnce(required)
    .mockRejectedValueOnce({ kind: "auth", message: "Invalid password" });
  await expect(invokeSensitive("start_linking", {})).rejects.toMatchObject({
    kind: "auth",
  });
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(prompt).toHaveBeenCalledTimes(1);
  invoke.mockRejectedValueOnce({ kind: "io", message: "Failed" });
  await expect(invokeSensitive("create_backup", {})).rejects.toMatchObject({
    kind: "io",
  });
  expect(prompt).toHaveBeenCalledTimes(1);
});

it("does not transfer an old challenge to a replacement prompt host", async () => {
  let reject!: (error: unknown) => void;
  unregister = registerSensitiveVerification(vi.fn());
  invoke.mockReturnValueOnce(
    new Promise((_resolve, fail) => {
      reject = fail;
    }),
  );
  const operation = invokeSensitive("start_linking", {});
  unregister();
  const replacement = vi.fn();
  unregister = registerSensitiveVerification(replacement);
  reject(required);
  await expect(operation).rejects.toMatchObject({
    kind: "authentication-canceled",
  });
  expect(replacement).not.toHaveBeenCalled();
});
