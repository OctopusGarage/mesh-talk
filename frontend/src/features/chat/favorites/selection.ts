import type { ChatMessage, Conversation } from "@/store/conversationModel";
import type { FavoriteSource } from "@/lib/api/messageFavorites";
export const MAX_FAVORITE_MESSAGES = 100;
export function canCollect(message: ChatMessage, now = Date.now()) {
  return (
    !!message.id &&
    message.id !== "0".repeat(64) &&
    !message.pending &&
    !message.failed &&
    !message.metadataPending &&
    !message.recalled &&
    (message.expiresAtMs == null || message.expiresAtMs > now)
  );
}
/** Keep selected snapshots when virtualized rows or older history pages unmount. */
export function toggleSelection(
  selected: ChatMessage[],
  message: ChatMessage,
): ChatMessage[] {
  if (selected.some((item) => item.id === message.id))
    return selected.filter((item) => item.id !== message.id);
  if (!canCollect(message) || selected.length >= MAX_FAVORITE_MESSAGES)
    return selected;
  return [...selected, message];
}
/** Core resolves canonical history order; client click order carries no authority. */
export function selectionSource(
  conversation: Conversation,
  selected: ChatMessage[],
): FavoriteSource {
  return {
    kind: conversation.kind,
    target: conversation.id,
    message_ids: selected.flatMap((message) =>
      message.id ? [message.id] : [],
    ),
  };
}
