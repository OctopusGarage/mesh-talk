import { useTranslation } from "react-i18next";
import { usePresence } from "@/store/presence";

/** Subscribes independently so presence ticks never redraw the message list. */
export function PresenceAvailability() {
  const unavailable = usePresence((s) => s.unavailable);
  const { t } = useTranslation();
  if (!unavailable) return null;
  return (
    <div
      role="status"
      className="rounded-md border bg-popover px-4 py-3 text-sm text-foreground"
    >
      {t("presence.unavailable")}
    </div>
  );
}
