import type { ChatMessage, Conversation } from "@/store/conversationModel";
import { isForwardedRecord } from "@/lib/forwardedRecord";
import { canCollect } from "../favorites/selection";
import type { ForwardSource } from "@/lib/api/messageForwarding";
export const MAX_FORWARD_MESSAGES = 100;
export function canForward(message: ChatMessage, now = Date.now()) {
  return (
    canCollect(message, now) &&
    (!message.file || !isForwardedRecord(message.file.name, message.file.mime))
  );
}
export function forwardSource(
  conversation: Conversation,
  messages: ChatMessage[],
): ForwardSource {
  if (
    !messages.length ||
    messages.length > MAX_FORWARD_MESSAGES ||
    messages.some((message) => !canForward(message))
  )
    throw new Error("Invalid forwarding selection");
  return {
    kind: conversation.kind,
    target: conversation.id,
    message_ids: messages.map((message) => message.id!),
  };
}
