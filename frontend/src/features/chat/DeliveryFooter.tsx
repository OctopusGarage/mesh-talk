import { useId } from "react";
import { useTranslation } from "react-i18next";
import { formatTime } from "@/lib/format";
import type { ChatMessage } from "@/store/chat";

/** Automatic receipt means the card reached its exact target, not download/read. */
export function DeliveryFooter({
  message,
  isChannel,
  showTime = true,
}: {
  message: ChatMessage;
  isChannel: boolean;
  showTime?: boolean;
}) {
  const { t } = useTranslation();
  const tooltip = useId();
  const delivery =
    message.fromMe && !isChannel && !message.pending && !message.failed
      ? message.delivery
      : undefined;
  const label = delivery ? t(`message.delivery.${delivery}`) : "";
  const detail = delivery
    ? t(`message.delivery.${message.file ? "fileHelp" : "help"}`)
    : "";
  const custody =
    message.fromMe &&
    isChannel &&
    message.id &&
    !message.file &&
    !message.pending &&
    !message.failed
      ? Math.min(message.replicas ?? 0, 2)
      : undefined;
  const custodyLabel =
    custody === undefined
      ? ""
      : t(
          `message.custody.${custody === 0 ? "local" : custody === 1 ? "one" : "multiple"}`,
        );
  if (!showTime && !message.pending && !delivery && custody === undefined)
    return null;
  return (
    <span className="mt-1 flex items-center gap-1 px-1 text-xs tabular-nums text-muted-foreground">
      {showTime && (
        <span data-testid="message-time">{formatTime(message.wallClock)}</span>
      )}
      {message.pending && <span>{` · ${t("message.sending")}`}</span>}
      {delivery && (
        <span
          className="group relative inline-flex items-center gap-1"
          tabIndex={0}
          role="img"
          aria-label={label}
          aria-describedby={tooltip}
          data-delivery={delivery}
        >
          <span aria-hidden="true">{delivery === "delivered" ? "✓" : "◷"}</span>
          {delivery === "awaiting" && <span aria-hidden="true">{label}</span>}
          <span
            id={tooltip}
            role="tooltip"
            className="pointer-events-none absolute bottom-full right-0 z-10 mb-1 hidden w-56 rounded border bg-popover p-2 font-sans text-xs text-popover-foreground shadow group-hover:block group-focus:block"
          >
            {label} · {detail}
          </span>
        </span>
      )}
      {custody !== undefined && (
        <span
          className="group relative inline-flex items-center gap-1"
          tabIndex={0}
          role="img"
          aria-label={custodyLabel}
          aria-describedby={tooltip}
          data-custody={custody}
        >
          <span aria-hidden="true">
            {custody === 0 ? "◷" : custody === 1 ? "✓" : "✓✓"}
          </span>
          {custody === 0 && <span aria-hidden="true">{custodyLabel}</span>}
          <span
            id={tooltip}
            role="tooltip"
            className="pointer-events-none absolute bottom-full right-0 z-10 mb-1 hidden w-56 rounded border bg-popover p-2 font-sans text-xs text-popover-foreground shadow group-hover:block group-focus:block"
          >
            {custodyLabel} · {t("message.custody.help")}
          </span>
        </span>
      )}
    </span>
  );
}
