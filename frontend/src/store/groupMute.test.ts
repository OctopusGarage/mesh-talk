import { expect, it } from "vitest";
import { groupAttentionCount } from "./groupMute";
it("keeps raw unread separate from muted attention", () => {
  expect(groupAttentionCount(8, true, 1)).toBe(1);
  expect(groupAttentionCount(8, true, 0)).toBe(0);
  expect(groupAttentionCount(8, false, 1)).toBe(8);
  expect(groupAttentionCount(1, true, 3)).toBe(1);
});
