import { useEffect, useRef, useState } from "react";
import { messageFavorites } from "@/lib/api/messageFavorites";
import { captureRuntimeOwner } from "@/store/ownership";
import { errorMessage } from "@/lib/error";
import type { ChatMessage, Conversation } from "@/store/conversationModel";
import type { FavoriteDraft } from "./FavoriteEditor";
import { canCollect, selectionSource, toggleSelection } from "./selection";

/** Scope-key this controller at the chat composition seam, not on virtual rows. */
export function useMessageCollection(
  conversation: Conversation | null,
  scope: string,
) {
  const [state, setState] = useState<{
    scope: string;
    selecting: boolean;
    selected: ChatMessage[];
    draft: FavoriteDraft | null;
    error: string | null;
    inspecting: boolean;
  }>({
    scope,
    selecting: false,
    selected: [],
    draft: null,
    error: null,
    inspecting: false,
  });
  const current =
    state.scope === scope
      ? state
      : {
          scope,
          selecting: false,
          selected: [],
          draft: null,
          error: null,
          inspecting: false,
        };
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const inspection = useRef(0);
  // Retire both stored selection and in-flight reads, even if this exact scope
  // becomes active again before the old IPC request resolves.
  if (state.scope !== scope) {
    inspection.current++;
    setState(current);
  }
  const update = (change: Partial<typeof current>) =>
    setState({ ...current, ...change });
  const cancel = () => {
    inspection.current++;
    update({
      selecting: false,
      selected: [],
      draft: null,
      inspecting: false,
      error: null,
    });
  };
  useEffect(() => {
    if ((!current.selecting && !current.inspecting) || current.draft) return;
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) {
        event.preventDefault();
        inspection.current++;
        setState({
          scope,
          selecting: false,
          selected: [],
          draft: null,
          error: null,
          inspecting: false,
        });
      }
    };
    document.addEventListener("keydown", escape);
    return () => document.removeEventListener("keydown", escape);
  }, [current.selecting, current.inspecting, current.draft, scope]);
  const collect = async (messages: ChatMessage[]) => {
    const lease = captureRuntimeOwner();
    if (
      !conversation ||
      !messages.length ||
      current.inspecting ||
      !lease.owner ||
      !lease.current()
    )
      return;
    const request = ++inspection.current;
    const source = selectionSource(conversation, messages);
    update({ inspecting: true, error: null });
    try {
      const result = await messageFavorites.inspect(lease.owner, source);
      if (
        !lease.current() ||
        currentScope.current !== scope ||
        request !== inspection.current
      )
        return;
      const existing = result.existing;
      const previews = existing
        ? existing.messages.map((message) => ({
            id: message.id,
            who: message.sender_name || message.who,
            text: message.text,
            wallClock: message.wall_clock,
            expiresAtMs: message.expires_at_ms,
            sticker: message.sticker,
            file: message.file
              ? { ...message.file, fileConv: message.file.file_conv }
              : null,
          }))
        : [...messages].sort((a, b) => a.wallClock - b.wallClock);
      update({
        inspecting: false,
        draft: {
          source: existing?.source ?? source,
          existing: existing ?? undefined,
          messages: previews,
        },
      });
    } catch (e) {
      if (
        lease.current() &&
        currentScope.current === scope &&
        request === inspection.current
      )
        update({ inspecting: false, error: errorMessage(e) });
    }
  };
  return {
    ...current,
    cancel,
    collect,
    toggle: (message: ChatMessage) =>
      update({ selected: toggleSelection(current.selected, message) }),
    start: (message: ChatMessage) =>
      update({
        selecting: true,
        selected: canCollect(message) ? [message] : [],
      }),
  };
}
