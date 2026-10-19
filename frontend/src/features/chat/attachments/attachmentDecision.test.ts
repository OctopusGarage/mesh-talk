import { describe, expect, it, vi } from "vitest";
import { createAttachmentDecision } from "./attachmentDecision";

function setup() {
  let current = true;
  const decideAttachment = vi.fn(async (_id: string, _accept: boolean) => {});
  const refreshAvailability = vi.fn(async (_ids: string[]) => {});
  const reportError = vi.fn();
  const decide = createAttachmentDecision({
    captureOwnership: () => ({ current: () => current }),
    decideAttachment,
    refreshAvailability,
    reportError,
  });
  return {
    decide,
    decideAttachment,
    refreshAvailability,
    reportError,
    retire: () => {
      current = false;
    },
  };
}

describe("attachment approval continuation", () => {
  it.each([true, false])(
    "submits consent %s before refreshing availability",
    async (accept) => {
      const s = setup();
      await s.decide("file", accept);
      expect(s.decideAttachment).toHaveBeenCalledWith("file", accept);
      expect(s.refreshAvailability).toHaveBeenCalledWith(["file"]);
      expect(s.decideAttachment.mock.invocationCallOrder[0]).toBeLessThan(
        s.refreshAvailability.mock.invocationCallOrder[0],
      );
      expect(s.reportError).not.toHaveBeenCalled();
    },
  );
  it.each(["retired owner", "missing metadata"])(
    "does not submit for %s",
    async (reason) => {
      const s = setup();
      if (reason === "retired owner") s.retire();
      await s.decide(reason === "missing metadata" ? "" : "file", true);
      expect(s.decideAttachment).not.toHaveBeenCalled();
    },
  );
  it("discards a decision completion after the captured session retires", async () => {
    const s = setup();
    let finish!: () => void;
    s.decideAttachment.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const pending = s.decide("file", true);
    s.retire();
    finish();
    await pending;
    expect(s.refreshAvailability).not.toHaveBeenCalled();
    expect(s.reportError).not.toHaveBeenCalled();
  });
  it("reports a failed availability refresh", async () => {
    const s = setup();
    const error = new Error("refresh unavailable");
    s.refreshAvailability.mockRejectedValue(error);
    await s.decide("file", true);
    expect(s.reportError).toHaveBeenCalledWith(error);
  });
  it.each(["decision", "refresh"])(
    "suppresses stale %s errors",
    async (stage) => {
      const s = setup();
      const fail = async () => {
        s.retire();
        throw new Error("old session");
      };
      if (stage === "decision") s.decideAttachment.mockImplementation(fail);
      else s.refreshAvailability.mockImplementation(fail);
      await s.decide("file", false);
      expect(s.reportError).not.toHaveBeenCalled();
    },
  );
  it("reports native approval failures without refreshing", async () => {
    const s = setup();
    const error = new Error("native approval failed");
    s.decideAttachment.mockRejectedValue(error);
    await s.decide("file", true);
    expect(s.reportError).toHaveBeenCalledWith(error);
    expect(s.refreshAvailability).not.toHaveBeenCalled();
  });
});
