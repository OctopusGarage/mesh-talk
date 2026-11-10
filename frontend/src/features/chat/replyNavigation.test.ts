import { beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  current: true,
  active: { kind: "account", id: "bob", name: "Bob" },
  activeRequest: 1,
  deleted: {} as Record<string, string[]>,
  open: vi.fn(),
  context: vi.fn(),
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "owner", current: () => state.current }),
}));
vi.mock("@/store/chat", () => ({ useChat: { getState: () => state } }));
vi.mock("@/lib/api", () => ({ chat: { searchContext: state.context } }));
import { openReplySource } from "./replyNavigation";
const reference = { kind: "account", target: "bob", message_id: "original" };
const row = {
  id: "original",
  text: "original",
  who: "bob",
  wall_clock: 1,
  from_me: false,
  reply_to: null,
};
beforeEach(() => {
  state.current = true;
  state.active = { kind: "account", id: "bob", name: "Bob" };
  state.activeRequest = 1;
  state.deleted = {};
  state.open.mockReset();
  state.context
    .mockReset()
    .mockResolvedValue({ status: "found", reference, messages: [row] });
});
it("uses the loaded viewport without fetching history", async () => {
  const jump = vi.fn(() => true);
  expect(await openReplySource("original", jump)).toBe("found");
  expect(jump).toHaveBeenCalledWith("original");
  expect(state.context).not.toHaveBeenCalled();
});
it("opens exact older context when the original is outside loaded history", async () => {
  expect(await openReplySource("original", () => false)).toBe("found");
  expect(state.context).toHaveBeenCalledWith("owner", reference);
  expect(state.open).toHaveBeenCalledWith(
    state.active,
    expect.objectContaining({
      reference,
      context: [expect.objectContaining({ id: "original" })],
    }),
  );
});
it.each([
  { status: "unavailable", reference, messages: [] },
  {
    status: "found",
    reference: { ...reference, target: "carol" },
    messages: [row],
  },
  { status: "found", reference, messages: [{ ...row, id: "duplicate" }] },
  { status: "found", reference, messages: [{ ...row, recalled: true }] },
  { status: "found", reference, messages: [{ ...row, expires_at_ms: 1 }] },
])(
  "does not substitute missing, mismatched or hidden sources: %j",
  async (context) => {
    state.context.mockResolvedValue(context);
    expect(await openReplySource("original", () => false)).toBe("unavailable");
    expect(state.open).not.toHaveBeenCalled();
  },
);
it("does not reopen locally deleted content", async () => {
  state.deleted = { "account:bob": ["original"] };
  expect(await openReplySource("original", () => false)).toBe("unavailable");
  expect(state.open).not.toHaveBeenCalled();
});
it.each(["owner", "conversation", "request"])(
  "discards results after %s retirement",
  async (kind) => {
    let current = true;
    state.context.mockImplementation(async () => {
      if (kind === "owner") state.current = false;
      if (kind === "conversation") state.activeRequest++;
      if (kind === "request") current = false;
      return { status: "found", reference, messages: [row] };
    });
    expect(
      await openReplySource(
        "original",
        () => false,
        () => current,
      ),
    ).toBe("retired");
    expect(state.open).not.toHaveBeenCalled();
  },
);
it("reports read failures as unavailable", async () => {
  state.context.mockRejectedValue(new Error("offline"));
  expect(await openReplySource("original", () => false)).toBe("unavailable");
});
