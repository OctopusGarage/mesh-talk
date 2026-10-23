import { expect, it, vi } from "vitest";
import { ConversationDrafts } from "./conversationDrafts";
const alice = "alice:1:account:a";
const bob = "alice:1:account:b";
function deferred() {
  let resolve!: (value: boolean) => void;
  const promise = new Promise<boolean>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
it("clears an accepted original draft while another conversation is open", async () => {
  const drafts = new ConversationDrafts();
  const job = deferred();
  drafts.set(alice, "Original draft");
  drafts.set(bob, "Other conversation");
  const admission = drafts.admit(alice, "Original draft", () => job.promise)!;
  expect(drafts.get(bob)).toBe("Other conversation");
  job.resolve(true);
  expect(await admission).toEqual({ accepted: true, cleared: true });
  expect(drafts.get(alice)).toBe("");
  expect(drafts.get(bob)).toBe("Other conversation");
});
it("keeps a pending admission across navigation away and remounting the original composer", async () => {
  const drafts = new ConversationDrafts();
  const job = deferred();
  const duplicate = vi.fn().mockResolvedValue(true);
  drafts.set(alice, "Still pending");
  const admission = drafts.admit(alice, "Still pending", () => job.promise)!;
  expect(drafts.isPending(bob)).toBe(false);
  // Remount reads the same scoped draft and its in-flight admission.
  expect(drafts.get(alice)).toBe("Still pending");
  expect(drafts.isPending(alice)).toBe(true);
  expect(drafts.admit(alice, "Still pending", duplicate)).toBeNull();
  expect(duplicate).not.toHaveBeenCalled();
  job.resolve(true);
  await admission;
  expect(drafts.isPending(alice)).toBe(false);
  expect(drafts.get(alice)).toBe("");
});
it("preserves a replacement draft edited after remount", async () => {
  const drafts = new ConversationDrafts();
  const job = deferred();
  drafts.set(alice, "Original draft");
  const admission = drafts.admit(alice, "Original draft", () => job.promise)!;
  drafts.set(alice, "Replacement draft");
  job.resolve(true);
  expect(await admission).toEqual({ accepted: true, cleared: false });
  expect(drafts.get(alice)).toBe("Replacement draft");
  expect(drafts.isPending(alice)).toBe(false);
});
it("does not erase a replacement even when it is edited back to the original text", async () => {
  const drafts = new ConversationDrafts();
  const job = deferred();
  drafts.set(alice, "Original draft");
  const admission = drafts.admit(alice, "Original draft", () => job.promise)!;
  drafts.set(alice, "Replacement draft");
  drafts.set(alice, "Original draft");
  job.resolve(true);
  expect(await admission).toEqual({ accepted: true, cleared: false });
  expect(drafts.get(alice)).toBe("Original draft");
});
it("allows another conversation and another owner generation to send independently", async () => {
  const drafts = new ConversationDrafts();
  const job = deferred();
  const otherOwner = "alice:2:account:a";
  for (const key of [alice, bob, otherOwner]) drafts.set(key, "Draft");
  const pending = drafts.admit(alice, "Draft", () => job.promise)!;
  expect(await drafts.admit(bob, "Draft", async () => true)).toEqual({
    accepted: true,
    cleared: true,
  });
  expect(await drafts.admit(otherOwner, "Draft", async () => true)).toEqual({
    accepted: true,
    cleared: true,
  });
  expect(drafts.get(alice)).toBe("Draft");
  job.resolve(false);
  await pending;
});
it("preserves rejected drafts, releases pending state, and never retries automatically", async () => {
  const drafts = new ConversationDrafts();
  const submit = vi.fn().mockRejectedValue(new Error("policy changed"));
  drafts.set(alice, "Keep this draft");
  expect(await drafts.admit(alice, "Keep this draft", submit)).toEqual({
    accepted: false,
    cleared: false,
  });
  expect(drafts.get(alice)).toBe("Keep this draft");
  expect(drafts.isPending(alice)).toBe(false);
  expect(submit).toHaveBeenCalledTimes(1);
});
it("refuses an obsolete composer snapshot instead of sending stale text", () => {
  const drafts = new ConversationDrafts();
  const submit = vi.fn().mockResolvedValue(true);
  drafts.set(alice, "Replacement draft");
  expect(drafts.admit(alice, "Original draft", submit)).toBeNull();
  expect(submit).not.toHaveBeenCalled();
});
it("preserves protected draft mode across navigation and only clears it on accepted original admission", async () => {
  const drafts = new ConversationDrafts();
  drafts.setProtection(alice, "authenticate");
  drafts.set(alice, "Secret");
  drafts.set(bob, "Other");
  expect(drafts.getProtection(alice)).toBe("authenticate");
  expect(drafts.getProtection(bob)).toBeUndefined();
  expect(await drafts.admit(alice, "Secret", async () => false)).toEqual({
    accepted: false,
    cleared: false,
  });
  expect(drafts.getProtection(alice)).toBe("authenticate");
  await drafts.admit(alice, "Secret", async () => true);
  expect(drafts.getProtection(alice)).toBeUndefined();
});
