import { expect, it } from "vitest";
import type { ChatMessage } from "@/store/conversationModel";
import { canCollect, toggleSelection, selectionSource } from "./selection";
const message = (id: string, extra = {}): ChatMessage => ({
  id,
  fromMe: false,
  who: "Alice",
  text: id,
  wallClock: 1,
  replyTo: null,
  ...extra,
});
it("keeps a selected message across history pages and deduplicates by canonical identity", () => {
  const first = message("one");
  const selected = toggleSelection([], first);
  expect(toggleSelection(selected, message("two"))).toEqual([
    first,
    message("two"),
  ]);
  expect(toggleSelection(selected, { ...first })).toEqual([]);
  expect(
    selectionSource({ kind: "account", id: "alice", name: "Alice" }, [
      message("two"),
      first,
    ]),
  ).toEqual({ kind: "account", target: "alice", message_ids: ["two", "one"] });
});
it("excludes pending, failed, recalled, expired and sentinel messages", () => {
  for (const extra of [
    { id: null },
    { id: "0".repeat(64) },
    { pending: true },
    { failed: true },
    { recalled: true },
    { metadataPending: true },
    { expiresAtMs: 1 },
  ])
    expect(canCollect(message("one", extra))).toBe(false);
  expect(canCollect(message("one"))).toBe(true);
});
it("caps a group at 100 while allowing deselection at the limit", () => {
  const selected = Array.from({ length: 100 }, (_, i) => message(String(i)));
  expect(toggleSelection(selected, message("extra"))).toBe(selected);
  expect(toggleSelection(selected, selected[0])).toHaveLength(99);
});

it.each(["click", "authenticate"] as const)(
  "excludes protected text and images from %s collection",
  (protection) => {
    for (const extra of [
      { text: "private text" },
      {
        file: {
          name: "private.png",
          size: 1,
          mime: "image/png",
          fileConv: "private-file",
          media: true,
        },
      },
    ]) {
      const protectedMessage = message("protected", { protection, ...extra });
      expect(canCollect(protectedMessage)).toBe(false);
      expect(toggleSelection([], protectedMessage)).toEqual([]);
      expect(() =>
        selectionSource({ kind: "account", id: "alice", name: "Alice" }, [
          message("ordinary"),
          protectedMessage,
        ]),
      ).toThrow("Protected messages");
    }
  },
);
