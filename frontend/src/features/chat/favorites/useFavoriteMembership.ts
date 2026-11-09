import { useEffect, useRef, useState } from "react";
import { startRecoveryRefresh } from "@/lib/recoveryRefresh";
import { readWithDeadline } from "@/lib/readDeadline";
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
  const published = useRef(state);
  const kind = conversation?.kind,
    target = conversation?.id;
  const requested = messages
    .flatMap((message) => (message.id ? [message.id] : []))
    .sort()
    .join(",");
  const expiries = messages
    .flatMap((message) => (message.expiresAtMs ? [message.expiresAtMs] : []))
    .sort((a, b) => a - b)
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
          const result = await readWithDeadline(() =>
            messageFavorites.inspect(lease.owner!, {
              kind,
              target,
              message_ids: ids.slice(offset, offset + 500),
            }),
          );
          if (!active || !lease.current()) return;
          result.saved_message_ids.forEach((id) => saved.add(id));
        }
        if (active && lease.current()) {
          const previous = published.current;
          if (
            previous?.scope !== scope ||
            previous.ids.size !== saved.size ||
            ![...saved].every((id) => previous.ids.has(id))
          ) {
            const next = { scope, ids: saved };
            published.current = next;
            setState(next);
          }
        }
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
    const stopRecovery = startRecoveryRefresh(() => void load(), {
      background: false,
    });
    let expiryTimer: ReturnType<typeof setTimeout> | undefined;
    const scheduleExpiry = () => {
      const next = expiries
        .split(",")
        .map(Number)
        .find((end) => end > Date.now());
      if (next === undefined) return;
      expiryTimer = setTimeout(
        () => {
          void load();
          scheduleExpiry();
        },
        Math.min(2_147_483_647, Math.max(1, next - Date.now())),
      );
    };
    scheduleExpiry();
    const stop = subscribeSyncInvalidation((change) => {
      if (change.owner === lease.owner && change.kind === "state") void load();
    });
    return () => {
      active = false;
      stopRecovery();
      clearTimeout(expiryTimer);
      stop();
    };
  }, [kind, target, requested, scope, expiries]);
  return state?.scope === scope ? state.ids : EMPTY;
}
