import { useEffect, useState } from "react";
import { messageFavorites } from "@/lib/api/messageFavorites";
import { subscribeSyncInvalidation } from "@/lib/syncInvalidation";
import { captureRuntimeOwner } from "@/store/ownership";
import type { ChatMessage, Conversation } from "@/store/conversationModel";
const EMPTY = new Set<string>();
/** Membership is independent from the collection browser's query and pages. */
export function useFavoriteMembership(
  conversation: Conversation | null,
  messages: ChatMessage[],
  scope: string,
) {
  const [state, setState] = useState<{
    scope: string;
    ids: Set<string>;
  } | null>(null);
  const kind = conversation?.kind,
    target = conversation?.id;
  const requested = messages
    .flatMap((message) => (message.id ? [message.id] : []))
    .sort()
    .join(",");
  useEffect(() => {
    const lease = captureRuntimeOwner();
    if (!kind || !target || !lease.owner || !lease.current()) return;
    let active = true,
      busy = false,
      dirty = false;
    const ids = requested ? requested.split(",") : [];
    const load = async () => {
      if (!active || !lease.current()) return;
      if (busy) {
        dirty = true;
        return;
      }
      busy = true;
      try {
        const saved = new Set<string>();
        for (let offset = 0; offset < ids.length; offset += 500) {
          const result = await messageFavorites.inspect(lease.owner!, {
            kind,
            target,
            message_ids: ids.slice(offset, offset + 500),
          });
          if (!active || !lease.current()) return;
          result.saved_message_ids.forEach((id) => saved.add(id));
        }
        if (active && lease.current()) setState({ scope, ids: saved });
      } catch {
        /* An unknown membership does not claim a message is saved. */
      } finally {
        busy = false;
        if (dirty && active) {
          dirty = false;
          void load();
        }
      }
    };
    void load();
    const timer = setInterval(() => void load(), 15_000);
    const stop = subscribeSyncInvalidation((change) => {
      if (change.owner === lease.owner && change.kind === "state") void load();
    });
    return () => {
      active = false;
      clearInterval(timer);
      stop();
    };
  }, [kind, target, requested, scope]);
  return state?.scope === scope ? state.ids : EMPTY;
}
