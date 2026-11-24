import { Clock } from "lucide-react";
import { useTranslation } from "react-i18next";
import { MessageStatus } from "./MessageStatus";
import { useDeliveryRecovery } from "./deliveryRecovery";
import type { ChatMessage } from "@/store/conversationModel";

/** Past local runtime evidence, independent of the sender's wall clock and receipt. */
function observationAge(
  observation: ChatMessage["deliveryObservation"],
  language: string,
) {
  if (
    !observation ||
    !["attempting", "retry_scheduled"].includes(observation.kind) ||
    !Number.isFinite(observation.ageMs) ||
    observation.ageMs < 0 ||
    !Number.isFinite(observation.sampledAt) ||
    observation.sampledAt < 0
  )
    return undefined;
  const elapsed = performance.now() - observation.sampledAt;
  if (!Number.isFinite(elapsed) || elapsed < 0) return undefined;
  const seconds = Math.floor((observation.ageMs + elapsed) / 1000);
  const [amount, unit] =
    seconds < 60
      ? ([seconds, "second"] as const)
      : seconds < 3600
        ? ([Math.floor(seconds / 60), "minute"] as const)
        : seconds < 86400
          ? ([Math.floor(seconds / 3600), "hour"] as const)
          : ([Math.floor(seconds / 86400), "day"] as const);
  return new Intl.RelativeTimeFormat(language, { numeric: "always" }).format(
    -amount,
    unit,
  );
}

export function DeliveryDetails({ message }: { message: ChatMessage }) {
  const { t, i18n } = useTranslation();
  const recover = useDeliveryRecovery();
  return (
    <MessageStatus
      label={t(
        message.file
          ? "message.delivery.fileAwaiting"
          : "message.delivery.awaiting",
      )}
      heading={t("message.delivery.detailsTitle")}
      delivery="awaiting"
      testId="delivery-details"
      icon={<Clock className="h-3 w-3" aria-hidden="true" />}
    >
      {({ close, trigger }) => {
        const age = observationAge(message.deliveryObservation, i18n.language);
        const reason = age ? message.deliveryObservation?.reason : undefined;
        const reasonKey =
          reason === "post_office_disabled"
            ? "postOfficeDisabled"
            : reason === "no_known_route"
              ? "noKnownRoute"
              : reason === "attempt_timed_out"
                ? "attemptTimedOut"
                : undefined;
        return (
          <>
            <p>{t("message.delivery.awaiting")}</p>
            <p>
              {age
                ? t(
                    `message.delivery.${message.deliveryObservation?.kind === "attempting" ? "attempted" : "retryScheduled"}`,
                    { age },
                  )
                : t("message.delivery.noDetails")}
            </p>
            {reasonKey && <p>{t(`message.delivery.${reasonKey}`)}</p>}
            <p>{t("message.delivery.automaticRetry")}</p>
            <p>
              {t(
                `message.delivery.${message.file ? "awaitingFileHelp" : "help"}`,
              )}
            </p>
            {recover && (
              <button
                type="button"
                className="rounded-sm text-left font-medium text-foreground underline underline-offset-4 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={() => {
                  close();
                  recover(
                    reason === "post_office_disabled"
                      ? "settings"
                      : "diagnostics",
                    trigger.current,
                  );
                }}
              >
                {t(
                  `message.delivery.${reason === "post_office_disabled" ? "reviewSettings" : "openDiagnostics"}`,
                )}
              </button>
            )}
          </>
        );
      }}
    </MessageStatus>
  );
}
