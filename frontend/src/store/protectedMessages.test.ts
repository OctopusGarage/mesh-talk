import { expect, it } from "vitest";
import { fromHistoryItem } from "./conversationState";
import type { HistoryItem } from "@/lib/types";
it("discards protected history plaintext and recalled text at the shared projection boundary", () => {
  const item = {
    id: "message",
    text: "secret",
    recalled_text: "secret",
    protection: "click",
    from_me: false,
    who: "peer",
    wall_clock: 1,
    reply_to: null,
    file: null,
    recalled: false,
    sticker: null,
  } as HistoryItem;
  expect(fromHistoryItem(item)).toMatchObject({
    text: "",
    recalledText: null,
    protection: "click",
  });
});
