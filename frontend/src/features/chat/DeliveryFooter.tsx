import { DeliveryDetails } from "./DeliveryDetails";
import { useAuth } from "@/store/auth";
import { Check, Copy, Inbox, Timer } from "lucide-react";
import { MessageStatus } from "./MessageStatus";
import { useChat } from "@/store/chat";
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
      ? message.replicas === undefined && message.postOfficeCopies === undefined
        ? "unknown"
        : Math.min(message.replicas ?? 0, 2) ||
          ((message.postOfficeCopies ?? 0) > 0 ? 1 : 0)
      : undefined;
  const postOfficeOnly =
    (message.replicas ?? 0) === 0 && (message.postOfficeCopies ?? 0) > 0;
  const custodyLabel =
    custody === undefined
      ? ""
      : custody === "unknown"
        ? t("message.custody.unknown")
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
    <div
      className={`mt-1 flex w-full min-w-0 max-w-full flex-wrap items-center gap-x-1 gap-y-0.5 px-1 text-[11px] tabular-nums text-muted-foreground ${message.fromMe ? "justify-end" : ""}`}
    >
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
        <MessageStatus
          key={`${detailsKey}:delivered`}
          label={label}
          delivery={delivery}
          icon={<Check className="h-3 w-3" aria-hidden="true" />}
        >
          {() => <p>{detail}</p>}
        </MessageStatus>
      )}
      {custody !== undefined && (
        <MessageStatus
          key={`${detailsKey}:custody:${custody}:${postOfficeOnly}`}
          label={custodyLabel}
          custody={custody}
          icon={
            postOfficeOnly ? (
              <Inbox className="h-3 w-3" aria-hidden="true" />
            ) : (
              <Copy className="h-3 w-3" aria-hidden="true" />
            )
          }
          count={
            custody === "unknown"
              ? "?"
              : !postOfficeOnly && custody > 0
                ? custody === 1
                  ? "1"
                  : "2+"
                : undefined
          }
        >
          {() => (
            <p>
              {t(
                custody === "unknown"
                  ? "message.custody.unknownHelp"
                  : postOfficeOnly
                    ? "message.custody.postOfficeHelp"
                    : "message.custody.help",
              )}
            </p>
          )}
        </MessageStatus>
      )}
    </div>
  );
}
