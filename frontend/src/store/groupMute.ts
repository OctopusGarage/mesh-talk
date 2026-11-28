/** Attention is a projection only; muting never marks a message read. */
export function groupAttentionCount(
  unread: number,
  muted: boolean,
  mentions: number,
): number {
  const count = Number.isSafeInteger(unread) && unread > 0 ? unread : 0;
  return muted ? Math.min(count, Math.max(0, mentions)) : count;
}
