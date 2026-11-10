import { useState, type ReactNode, type RefObject } from "react";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { DeliveryRecoveryContext } from "./deliveryRecovery";
import { SettingsDialog } from "./SettingsDialog";
import { DiagnosticsDialog } from "./DiagnosticsDialog";

function currentScope() {
  const auth = useAuth.getState();
  const chat = useChat.getState();
  return `${auth.user?.id}:${auth.generation}:${chat.runEpoch}:${chat.identityEpoch}:${chat.active?.kind}:${chat.active?.id}`;
}

/** Dialog lifetime follows the conversation/session, independently of virtualized rows. */
export function DeliveryRecoveryProvider({
  children,
  fallbackRef,
}: {
  children: ReactNode;
  fallbackRef?: RefObject<HTMLElement | null>;
}) {
  const authScope = useAuth((s) => `${s.user?.id}:${s.generation}`);
  const chatScope = useChat(
    (s) => `${s.runEpoch}:${s.identityEpoch}:${s.active?.kind}:${s.active?.id}`,
  );
  const scope = `${authScope}:${chatScope}`;
  const [state, setState] = useState<{
    scope: string;
    request: {
      target: "settings" | "diagnostics";
      origin: HTMLElement | null;
    } | null;
  }>({ scope, request: null });
  if (state.scope !== scope) setState({ scope, request: null });
  const request = state.scope === scope ? state.request : null;
  const close = (open: boolean) => {
    if (!open) setState({ scope, request: null });
  };
  const restoreFocus = (event: Event) => {
    event.preventDefault();
    if (scope !== currentScope()) return;
    (request?.origin?.isConnected
      ? request.origin
      : fallbackRef?.current
    )?.focus();
  };
  return (
    <DeliveryRecoveryContext.Provider
      value={(target, origin) =>
        setState({ scope, request: { target, origin } })
      }
    >
      {children}
      {request?.target === "settings" && (
        <SettingsDialog
          open
          onOpenChange={close}
          onCloseAutoFocus={restoreFocus}
          initialTarget={{
            category: "connection",
            controlId: "setting-post-office",
          }}
        />
      )}
      {request?.target === "diagnostics" && (
        <DiagnosticsDialog
          open
          onOpenChange={close}
          onCloseAutoFocus={restoreFocus}
        />
      )}
    </DeliveryRecoveryContext.Provider>
  );
}
