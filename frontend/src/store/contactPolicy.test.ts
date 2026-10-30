import { beforeEach, describe, expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
const runtime = vi.hoisted(() => ({ accountId: "root-a" }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("./ownership", () => ({
  currentRuntimeAccountId: () => runtime.accountId,
}));
import { useContactPolicy } from "./contactPolicy";

beforeEach(() => {
  invoke.mockReset();
  runtime.accountId = "root-a";
  useContactPolicy.getState().reset();
});
const snapshot = (owner = "alice", hidden = true) => ({
  owner,
  contacts: hidden ? [{ account_id: "bob", name: "Bob" }] : [],
});
describe("account-scoped contact visibility", () => {
  it("exposes a stalled policy read for retry and ignores its late snapshot", async () => {
    vi.useFakeTimers();
    let finish!: (value: unknown) => void;
    invoke.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const pending = useContactPolicy.getState().load("alice");
    try {
      await vi.advanceTimersByTimeAsync(10_001);
      expect(useContactPolicy.getState().loading).toBe(false);
      expect(useContactPolicy.getState().error).toBe("load");
      invoke.mockResolvedValue(snapshot("alice", false));
      await useContactPolicy.getState().load("alice");
      finish(snapshot());
      await pending;
      expect(useContactPolicy.getState().contacts).toEqual({});
    } finally {
      finish(snapshot());
      await pending;
      vi.useRealTimers();
    }
  });
  it("forwards the captured full account when reopening a contact", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    invoke.mockResolvedValue(snapshot("alice", false));
    expect(
      await useContactPolicy
        .getState()
        .change("alice", "bob", false, "Bob", "root-a"),
    ).toBe(true);
    expect(invoke).toHaveBeenLastCalledWith("set_contact_hidden", {
      owner: "alice",
      account: "bob",
      hidden: false,
      name: "Bob",
      expectedAccount: "root-a",
    });
  });
  it("keeps replacement-account policy when an old reopen completes", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const reopening = useContactPolicy
      .getState()
      .change("alice", "bob", false, "Bob", "root-a");
    runtime.accountId = "root-b";
    resolve(snapshot("alice", false));
    expect(await reopening).toBe(false);
    expect(useContactPolicy.getState().contacts.bob).toBeDefined();
    expect(useContactPolicy.getState().busy).toBe(false);
  });
  it("loads saved visibility without mutating the raw roster", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    expect(useContactPolicy.getState().contacts.bob.name).toBe("Bob");
    expect(useContactPolicy.getState().loaded).toBe(true);
    expect(invoke).toHaveBeenCalledWith("get_hidden_contacts", {
      owner: "alice",
    });
  });
  it("discards late loads from the previous login", async () => {
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const old = useContactPolicy.getState().load("alice");
    invoke.mockResolvedValue(snapshot("carol", false));
    await useContactPolicy.getState().load("carol");
    resolve(snapshot());
    await old;
    expect(useContactPolicy.getState().owner).toBe("carol");
    expect(useContactPolicy.getState().contacts).toEqual({});
  });
  it("keeps saved policy on write failure and exposes the failure", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    invoke.mockRejectedValue(new Error("disk full"));
    expect(
      await useContactPolicy.getState().change("alice", "bob", false, "Bob"),
    ).toBe(false);
    expect(useContactPolicy.getState().contacts.bob).toBeDefined();
    expect(useContactPolicy.getState().error).toBeTruthy();
    expect(useContactPolicy.getState().busy).toBe(false);
  });
  it("does not silently show all contacts after a failed initial load", async () => {
    invoke.mockRejectedValue(new Error("corrupt file"));
    await useContactPolicy.getState().load("alice");
    expect(useContactPolicy.getState().loaded).toBe(false);
    expect(useContactPolicy.getState().error).toBeTruthy();
  });
  it("rejects a mismatched response and clears policy on reset", async () => {
    invoke.mockResolvedValue(snapshot("other"));
    await useContactPolicy.getState().load("alice");
    expect(useContactPolicy.getState().loaded).toBe(false);
    useContactPolicy.getState().reset();
    expect(useContactPolicy.getState().owner).toBeNull();
  });
  it("discards writes completing after logout or another login", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const change = useContactPolicy
      .getState()
      .change("alice", "bob", false, "Bob");
    useContactPolicy.getState().reset();
    resolve(snapshot("alice", false));
    expect(await change).toBe(false);
    expect(useContactPolicy.getState().owner).toBeNull();
    expect(useContactPolicy.getState().loaded).toBe(false);
  });
  it("allows a new login to load while an old user's write is pending", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const oldWrite = useContactPolicy
      .getState()
      .change("alice", "bob", false, "Bob");
    invoke.mockResolvedValue(snapshot("carol", false));
    await useContactPolicy.getState().load("carol");
    resolve(snapshot("alice", false));
    expect(await oldWrite).toBe(false);
    expect(useContactPolicy.getState().owner).toBe("carol");
    expect(useContactPolicy.getState().loaded).toBe(true);
    expect(useContactPolicy.getState().contacts).toEqual({});
  });
  it("serializes mutations and refuses stale-owner requests", async () => {
    invoke.mockResolvedValue(snapshot());
    await useContactPolicy.getState().load("alice");
    expect(
      await useContactPolicy.getState().change("carol", "bob", false, "Bob"),
    ).toBe(false);
    let resolve!: (value: unknown) => void;
    invoke.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const write = useContactPolicy
      .getState()
      .change("alice", "bob", false, "Bob");
    expect(
      await useContactPolicy
        .getState()
        .change("alice", "another", true, "Another"),
    ).toBe(false);
    resolve(snapshot("alice", false));
    expect(await write).toBe(true);
    expect(useContactPolicy.getState().contacts).toEqual({});
  });
});
