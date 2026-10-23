import { useTranslation } from "react-i18next";
import { ChevronDown, Info } from "lucide-react";
import { usePresence } from "@/store/presence";

/** Subscribes independently so presence ticks never redraw the message list. */
export function PresenceAvailability() {
  const unavailable = usePresence((s) => s.unavailable);
  const failure = usePresence((s) => s.failure);
  const { t } = useTranslation();
  if (!unavailable) return null;
  return (
    <details
      data-testid="presence-availability"
      className="group mt-2 min-w-0 border-t pt-1 text-xs text-muted-foreground"
    >
      <summary className="flex min-h-9 cursor-pointer list-none items-center gap-2 rounded-md px-1 py-1.5 hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <Info aria-hidden="true" className="h-3.5 w-3.5 shrink-0" />
        <span role="status" className="min-w-0 flex-1 leading-5">
          {t("presence.unavailable")}
        </span>
        <ChevronDown
          aria-hidden="true"
          className="h-3.5 w-3.5 shrink-0 group-open:rotate-180"
        />
      </summary>
      <div className="max-h-[min(9rem,20vh)] overflow-y-auto overscroll-contain px-1 pb-1 pt-1 leading-5 [overflow-wrap:anywhere]">
        <p>{t("presence.unavailableDetail")}</p>
        {failure && (
          <p data-testid="presence-failure-detail" className="mt-2 select-text">
            <span className="block font-medium text-foreground">
              {t("presence.latestError")}
            </span>
            {failure.kind === "request"
              ? failure.message
              : t(
                  failure.kind === "timeout"
                    ? "presence.requestTimeout"
                    : "presence.invalidResponse",
                )}
          </p>
        )}
      </div>
    </details>
  );
}
