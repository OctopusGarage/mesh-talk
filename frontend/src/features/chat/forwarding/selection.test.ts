import { expect, it } from "vitest";
import { canForward, forwardSource } from "./selection";
import type { ChatMessage } from "@/store/conversationModel";
const message = {
  id: "message",
  text: "hello",
  wallClock: 1,
  who: "alice",
  fromMe: false,
} as ChatMessage;
it("rejects protected, retired and unaccepted messages", () => {
  expect(canForward(message)).toBe(true);
  for (const change of [
    { protection: { mode: "click" } },
    { pending: true },
    { recalled: true },
    { failed: true },
    { expiresAtMs: 1 },
  ])
    expect(canForward({ ...message, ...change } as ChatMessage)).toBe(false);
});
it("passes canonical source identities without renderer content", () => {
  expect(
    forwardSource({ kind: "account", id: "alice", name: "Alice" }, [message]),
  ).toEqual({ kind: "account", target: "alice", message_ids: ["message"] });
});

it("does not offer nested chat records as forwardable sources", () => {
  expect(
    canForward({
      ...message,
      file: {
        name: "history.mtchat",
        mime: "application/octet-stream",
        fileConv: "archive",
        size: 1,
        media: false,
      },
    }),
  ).toBe(false);
});
