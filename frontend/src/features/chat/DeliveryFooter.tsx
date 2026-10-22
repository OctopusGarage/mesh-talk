import { DeliveryDetails } from "./DeliveryDetails";
import { useAuth } from "@/store/auth";
import { Timer } from "lucide-react";
import { useChat } from "@/store/chat";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { formatTime } from "@/lib/format";
import type { ChatMessage } from "@/store/chat";

/** Automatic receipt means the card reached its exact target, not download/read. */
export function DeliveryFooter({
  message,
  isChannel,
  isPersonal = false,
  showTime = true,
}: {
  message: ChatMessage;
  isChannel: boolean;
  isPersonal?: boolean;
  showTime?: boolean;
}) {
  const { t } = useTranslation();
  const tooltip = useId();
  // A different message, conversation or session owns a fresh, closed disclosure.
  const authScope = useAuth((s) => `${s.user?.id ?? ""}:${s.generation}`);
  const chatScope = useChat(
    (s) => `${s.runEpoch}:${s.identityEpoch}:${s.active?.kind}:${s.active?.id}`,
  );
  const detailsKey = `${authScope}:${chatScope}:${message.id}:${message.clientId}`;
  const clock = useChat((state) =>
    message.expiresAtMs == null ? 0 : state.expiryNow,
  );
  const seconds =
    message.expiresAtMs == null
      ? null
      : Math.max(
          0,
          Math.ceil((message.expiresAtMs - Math.max(clock, Date.now())) / 1000),
        );
  const remaining =
    seconds == null
      ? ""
      : `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  const delayed =
    !message.fromMe &&
    message.arrivalKind === "catch_up" &&
    message.receivedAt !== undefined;
  const receiptDetail = delayed
    ? t("conversation.receivedLocally", {
        time: new Date(message.receivedAt!).toLocaleString(),
      })
    : undefined;
  const delivery =
    message.fromMe &&
    !isChannel &&
    !isPersonal &&
    !message.pending &&
    !message.failed &&
    !message.recalled &&
    !!message.id
      ? message.delivery
      : undefined;
  const label = delivery
    ? t(
        `message.delivery.${message.file ? (delivery === "delivered" ? "fileDelivered" : "fileAwaiting") : delivery}`,
      )
    : "";
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
      ? Math.min(message.replicas ?? 0, 2) ||
        ((message.postOfficeCopies ?? 0) > 0 ? 1 : 0)
      : undefined;
  const postOfficeOnly =
    (message.replicas ?? 0) === 0 && (message.postOfficeCopies ?? 0) > 0;
  const custodyLabel =
    custody === undefined
      ? ""
      : postOfficeOnly
        ? t("message.custody.postOffice")
        : t(
            `message.custody.${custody === 0 ? "local" : custody === 1 ? "one" : "multiple"}`,
          );
  const saved =
    isPersonal &&
    message.fromMe &&
    !!message.id &&
    !message.pending &&
    !message.failed;
  if (
    seconds === null &&
    !delayed &&
    !showTime &&
    !message.pending &&
    !delivery &&
    custody === undefined &&
    !saved
  )
    return null;
  return (
    <span className="mt-1 flex items-center gap-1 px-1 text-xs tabular-nums text-muted-foreground">
      {seconds !== null && (
        <span
          data-testid="message-expiry"
          title={t("message.expiresIn", { time: remaining })}
          aria-label={t("message.expiresIn", { time: remaining })}
          className="inline-flex items-center gap-1"
        >
          <Timer className="h-3 w-3" aria-hidden="true" />
          <span>{remaining}</span>
        </span>
      )}
      {delayed && (
        <span
          title={receiptDetail}
          aria-label={receiptDetail}
          data-testid="delayed-message"
        >
          {t("conversation.delayedMessage")}
        </span>
      )}
      {showTime && (
        <span data-testid="message-time">{formatTime(message.wallClock)}</span>
      )}
      {message.pending && <span>{` · ${t("message.sending")}`}</span>}
      {saved && (
        <span data-personal-saved>
          {t(message.file ? "personal.fileCardSaved" : "personal.saved")}
        </span>
      )}
      {delivery === "awaiting" && (
        <DeliveryDetails key={detailsKey} message={message} />
      )}
      {delivery === "delivered" && (
        <span
          className="group relative inline-flex items-center gap-1"
          tabIndex={0}
          role="img"
          aria-label={label}
          aria-describedby={tooltip}
          data-delivery={delivery}
        >
          <span aria-hidden="true">{delivery === "delivered" ? "✓" : "◷"}</span>
          {(delivery === "awaiting" || message.file) && (
            <span aria-hidden="true">{label}</span>
          )}
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
          {(custody === 0 || postOfficeOnly) && (
            <span aria-hidden="true">{custodyLabel}</span>
          )}
          <span
            id={tooltip}
            role="tooltip"
            className="pointer-events-none absolute bottom-full right-0 z-10 mb-1 hidden w-56 rounded border bg-popover p-2 font-sans text-xs text-popover-foreground shadow group-hover:block group-focus:block"
          >
            {custodyLabel} ·{" "}
            {t(
              postOfficeOnly
                ? "message.custody.postOfficeHelp"
                : "message.custody.help",
            )}
          </span>
        </span>
      )}
    </span>
  );
}
