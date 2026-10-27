import { useId, useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
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

export function DeliveryDetails({
  message,
  compact = false,
}: {
  message: ChatMessage;
  compact?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  const recover = useDeliveryRecovery();
  const origin = useRef<HTMLButtonElement>(null);
  const navigating = useRef(false);
  const title = useId();
  const description = useId();
  const age = open
    ? observationAge(message.deliveryObservation, i18n.language)
    : undefined;
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
      <Popover
        open={open}
        onOpenChange={(next) => {
          if (next) navigating.current = false;
          setOpen(next);
        }}
        modal={false}
      >
        <PopoverTrigger asChild>
          <button
            type="button"
            ref={origin}
            data-testid="delivery-details-trigger"
            data-delivery="awaiting"
            aria-label={t("message.delivery.awaiting")}
            title={compact ? t("message.delivery.awaiting") : undefined}
            className="inline-flex min-h-6 items-center gap-1 rounded-sm text-left underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <span aria-hidden="true">◷</span>
            <span className={compact ? "sr-only" : undefined}>
              {t("message.delivery.awaiting")}
            </span>
          </button>
        </PopoverTrigger>
        <PopoverContent
          data-testid="delivery-details"
          onCloseAutoFocus={(event) => {
            if (navigating.current) event.preventDefault();
          }}
          align="end"
          aria-labelledby={title}
          aria-describedby={description}
          className="w-72 max-w-[calc(100vw-16px)] overflow-y-auto break-words p-3 text-sm"
          style={{ maxHeight: "var(--radix-popover-content-available-height)" }}
        >
          <h3 id={title} className="font-semibold">
            {t("message.delivery.detailsTitle")}
          </h3>
          <div id={description} className="mt-2 space-y-2">
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
            <p className="text-xs text-muted-foreground">
              {t(
                `message.delivery.${message.file ? "awaitingFileHelp" : "help"}`,
              )}
            </p>
          </div>
          {recover && (
            <button
              type="button"
              className="mt-3 rounded-sm text-left font-medium text-foreground underline underline-offset-4 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onClick={() => {
                navigating.current = true;
                setOpen(false);
                recover(
                  reason === "post_office_disabled"
                    ? "settings"
                    : "diagnostics",
                  origin.current,
                );
              }}
            >
              {t(
                `message.delivery.${reason === "post_office_disabled" ? "reviewSettings" : "openDiagnostics"}`,
              )}
            </button>
          )}
        </PopoverContent>
      </Popover>
    </>
  );
}
