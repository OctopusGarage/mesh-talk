import { describe, expect, it, vi } from "vitest";
import { createRosterObservation } from "./rosterObservation";

describe("roster observation ticket", () => {
  it("starts only on demand and shares one actual read and original timestamp", async () => {
    const read = vi.fn(async () => ({ peers: [], people: {}, identity: {} }));
    const ticket = createRosterObservation("alice", read);
    expect(read).not.toHaveBeenCalled();
    const first = ticket.read("alice");
    const second = ticket.read("alice");
    expect(first).toBe(second);
    expect(read).toHaveBeenCalledTimes(1);
    expect((await first).requestedAt).toBe((await second).requestedAt);
  });
  it("rejects another owner without starting native work", async () => {
    const read = vi.fn(async () => ({ peers: [], people: {}, identity: {} }));
    const ticket = createRosterObservation("alice", read);
    await expect(ticket.read("bob")).rejects.toThrow("owner");
    expect(read).not.toHaveBeenCalled();
  });
  it("does not reuse requests across invalidation tickets", async () => {
    const read = vi.fn(async () => ({ peers: [], people: {}, identity: {} }));
    await createRosterObservation("alice", read).read("alice");
    await createRosterObservation("alice", read).read("alice");
    expect(read).toHaveBeenCalledTimes(2);
  });
  it("rejects a retired runtime before demand without orphan native work", async () => {
    let current = true;
    const read = vi.fn(async () => "snapshot");
    const ticket = createRosterObservation("alice", read, () => current);
    current = false;
    await expect(ticket.read("alice")).rejects.toThrow("retired");
    expect(read).not.toHaveBeenCalled();
  });
  it("rejects a completion from a replaced runtime", async () => {
    let current = true;
    let finish!: (value: string) => void;
    const ticket = createRosterObservation(
      "alice",
      () =>
        new Promise<string>((resolve) => {
          finish = resolve;
        }),
      () => current,
    );
    const pending = ticket.read("alice");
    current = false;
    finish("retired snapshot");
    await expect(pending).rejects.toThrow("retired");
  });
});
