import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { NotificationPermissionHelp } from "./NotificationPermissionHelp";

export function NotificationPermissionNotice({
  onDismiss,
}: {
  onDismiss: () => void;
}) {
  const { t } = useTranslation();
  return (
    <aside
      aria-label={t("notificationPermission.reminder")}
      data-testid="notification-permission-notice"
      className="shrink-0 border-b border-border bg-card px-4 py-3"
    >
      <div className="mx-auto flex max-w-3xl items-start gap-3">
        <div className="min-w-0 flex-1">
          <NotificationPermissionHelp message />
        </div>
        <button
          type="button"
          onClick={onDismiss}
          aria-label={t("notificationPermission.dismiss")}
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
        >
          <X className="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
    </aside>
  );
}
