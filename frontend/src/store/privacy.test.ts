import { beforeEach, describe, expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { usePrivacy } from "./privacy";

const bob = "b".repeat(32);
const snapshot = (owner = "alice", invisible = true) => ({
  owner,
  version: 1,
  invisible,
  allowed_accounts: [{ id: bob, name: "Bob", source: "Manual" }],
});
beforeEach(() => {
  invoke.mockReset();
  usePrivacy.getState().reset();
});
describe("invisible-mode session policy", () => {
  it("rejects IDs that only become valid through string coercion", async () => {
    invoke.mockResolvedValue({
      ...snapshot(),
      allowed_accounts: [{ id: [bob], name: "Bob", source: "Manual" }],
    });
    await usePrivacy.getState().load("alice");
    expect(usePrivacy.getState().loaded).toBe(false);
    expect(usePrivacy.getState().error).toBe("load");
  });
  it("disables changes after a failed refresh without losing the last displayed snapshot", async () => {
    invoke.mockResolvedValue(snapshot());
    await usePrivacy.getState().load("alice");
    invoke.mockRejectedValue(new Error("node stopped"));
    await usePrivacy.getState().load("alice");
    expect(usePrivacy.getState().loaded).toBe(false);
    expect(usePrivacy.getState().snapshot?.invisible).toBe(true);
    expect(await usePrivacy.getState().setInvisible("alice", false)).toBe(
      false,
    );
  });
  it("loads durable state through owner-bound IPC", async () => {
    invoke.mockResolvedValue(snapshot());
    await usePrivacy.getState().load("alice");
    expect(invoke).toHaveBeenCalledWith("get_privacy", { owner: "alice" });
    expect(usePrivacy.getState().snapshot?.invisible).toBe(true);
    expect(usePrivacy.getState().loaded).toBe(true);
  });
  it("does not display public mode as loaded after an initial error", async () => {
    invoke.mockRejectedValue(new Error("corrupt"));
    await usePrivacy.getState().load("alice");
    expect(usePrivacy.getState().loaded).toBe(false);
    expect(usePrivacy.getState().snapshot).toBeNull();
    expect(usePrivacy.getState().error).toBe("load");
  });
  it("retains committed state until a successful mode change", async () => {
    invoke.mockResolvedValue(snapshot());
    await usePrivacy.getState().load("alice");
    invoke.mockRejectedValue(new Error("disk full"));
    expect(await usePrivacy.getState().setInvisible("alice", false)).toBe(
      false,
    );
    expect(usePrivacy.getState().snapshot?.invisible).toBe(true);
    expect(usePrivacy.getState().error).toBe("save");
    expect(usePrivacy.getState().busy).toBe(false);
  });
  it("revokes using account identity and accepts the committed snapshot", async () => {
    invoke.mockResolvedValue(snapshot());
    await usePrivacy.getState().load("alice");
    invoke.mockResolvedValue({ ...snapshot(), allowed_accounts: [] });
    expect(await usePrivacy.getState().setAllowed("alice", bob, false)).toBe(
      true,
    );
    expect(invoke).toHaveBeenLastCalledWith("set_privacy_allowed", {
      owner: "alice",
      account: bob,
      allowed: false,
    });
    expect(usePrivacy.getState().snapshot?.allowed_accounts).toEqual([]);
  });
  it("ignores the prior login's late load", async () => {
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const old = usePrivacy.getState().load("alice");
    invoke.mockResolvedValue(snapshot("carol", false));
    await usePrivacy.getState().load("carol");
    resolve(snapshot());
    await old;
    expect(usePrivacy.getState().owner).toBe("carol");
    expect(usePrivacy.getState().snapshot?.invisible).toBe(false);
  });
  it("ignores a mode change completing after logout", async () => {
    invoke.mockResolvedValue(snapshot());
    await usePrivacy.getState().load("alice");
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const old = usePrivacy.getState().setInvisible("alice", false);
    usePrivacy.getState().reset();
    resolve(snapshot("alice", false));
    expect(await old).toBe(false);
    expect(usePrivacy.getState().snapshot).toBeNull();
  });
  it("rejects malformed or wrong-owner snapshots", async () => {
    for (const bad of [
      snapshot("other"),
      { ...snapshot(), version: 2 },
      { ...snapshot(), invisible: "yes" },
      {
        ...snapshot(),
        allowed_accounts: [{ id: "bad", name: "Bob", source: "Manual" }],
      },
      {
        ...snapshot(),
        allowed_accounts: [{ id: bob, name: "Bob", source: "Unknown" }],
      },
    ]) {
      usePrivacy.getState().reset();
      invoke.mockResolvedValue(bad);
      await usePrivacy.getState().load("alice");
      expect(usePrivacy.getState().loaded).toBe(false);
    }
  });
});
