import { useEffect, useMemo, useRef } from "react";
import type { ChatMessage } from "@/store/chat";
import { messageRowKey } from "./useConversationViewport";

/** Commit-scoped arrival evidence survives Virtuoso's deferred row mounts. */
export function useMessageArrivalMotion(
  scope: string,
  messages: ChatMessage[],
  suppressed = false,
) {
  const committed = useRef({
    scope,
    seen: new Set<string>(),
    candidates: new Set<string>(),
    mounted: new Set<string>(),
    primed: false,
  });
  const baseline = committed.current;
  const eligible = baseline.scope === scope && baseline.primed && !suppressed;
  const candidates = useMemo(() => {
    const next = new Set(eligible ? baseline.candidates : []);
    if (eligible)
      messages.forEach((message, index) => {
        if (
          !baseline.seen.has(messageRowKey(message, index)) &&
          (message.arrivalKind === "live" ||
            (message.fromMe && message.pending === true && !!message.clientId))
        )
          next.add(messageRowKey(message, index));
      });
    return next;
  }, [eligible, baseline, messages]);
  useEffect(() => {
    const previous = committed.current;
    const seen = new Set(previous.scope === scope ? previous.seen : []);
    messages.forEach((message, index) =>
      seen.add(messageRowKey(message, index)),
    );
    committed.current = {
      scope,
      seen,
      candidates,
      mounted: previous.scope === scope ? previous.mounted : new Set<string>(),
      primed:
        (previous.scope === scope && previous.primed) || messages.length > 0,
    };
  }, [messages, scope, candidates]);
  const isFresh = (message: ChatMessage, index: number) =>
    eligible &&
    candidates.has(messageRowKey(message, index)) &&
    !baseline.mounted.has(messageRowKey(message, index));
  isFresh.mounted = (message: ChatMessage, index: number) => {
    baseline.mounted.add(messageRowKey(message, index));
  };
  return isFresh;
}
