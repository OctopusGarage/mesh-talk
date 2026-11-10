import { chat } from "@/lib/api";
import { useChat } from "@/store/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import {
  convKey,
  fromHistoryItem,
  sameSearchReference,
} from "@/store/conversationState";

/** Reuse exact history context; reply navigation never guesses by text or timestamp. */
export async function openReplySource(
  id: string,
  jumpToMessage: (id: string) => boolean,
  stillCurrent: () => boolean = () => true,
): Promise<"found" | "unavailable" | "retired"> {
  const lease = captureRuntimeOwner();
  const { active, activeRequest } = useChat.getState();
  const current = () =>
    lease.current() &&
    stillCurrent() &&
    useChat.getState().activeRequest === activeRequest;
  if (!active || !current()) return "retired";
  if (jumpToMessage(id)) return "found";
  const reference = { kind: active.kind, target: active.id, message_id: id };
  try {
    const context = await chat.searchContext(lease.owner!, reference);
    if (!current()) return "retired";
    if (
      context.status !== "found" ||
      !sameSearchReference(reference, context.reference) ||
      useChat.getState().deleted[convKey(active)]?.includes(id) ||
      !context.messages.some(
        (row) =>
          row.id === id &&
          !row.recalled &&
          (row.expires_at_ms == null || row.expires_at_ms > Date.now()),
      )
    )
      return "unavailable";
    void useChat.getState().open(active, {
      reference,
      context: context.messages.map(fromHistoryItem),
    });
    return "found";
  } catch {
    return current() ? "unavailable" : "retired";
  }
}
