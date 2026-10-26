import { useEffect, useRef, useState } from "react";
import { BellOff, Check, ExternalLink } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { notifications } from "@/lib/api/notifications";
import { useNotificationPermission } from "@/lib/notify";

/** One presentation of OS authorization, reused by settings and the reminder. */
export function NotificationPermissionHelp({
  message = false,
}: {
  message?: boolean;
}) {
  const { t } = useTranslation();
  const active = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);
  const permission = useNotificationPermission((s) => s.permission);
  const checking = useNotificationPermission((s) => s.checking);
  const requesting = useNotificationPermission((s) => s.requesting);
  const refresh = useNotificationPermission((s) => s.refresh);
  const enable = useNotificationPermission((s) => s.enable);
  const [opening, setOpening] = useState(false);
  const [openFailed, setOpenFailed] = useState(false);
  const [opened, setOpened] = useState(false);
  const busy = checking || requesting || opening;
  const status = permission?.status;
  const allowed = status === "granted";
  const title = t(`notificationPermission.${status ?? "checking"}`);
  const instructions = t(
    `notificationPermission.instructions.${permission?.platform ?? "other"}`,
  );
  return (
    <div
      data-testid="notification-permission"
      className="min-w-0 space-y-2 text-[12px] leading-5"
    >
      <div role="status" aria-live="polite" className="flex items-start gap-2">
        {allowed ? (
          <Check
            className="mt-0.5 h-4 w-4 shrink-0 text-primary"
            aria-hidden="true"
          />
        ) : (
          <BellOff
            className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
        )}
        <div className="min-w-0">
          <p className="font-medium text-foreground">{title}</p>
          <p className="text-muted-foreground">
            {t(
              `notificationPermission.${allowed ? "allowedHint" : message ? "messageHint" : "settingsHint"}`,
            )}
          </p>
        </div>
      </div>
      {!allowed && status && (
        <div className="space-y-2 pl-6">
          <p className="text-muted-foreground">{instructions}</p>
          <div className="flex flex-wrap items-center gap-1.5">
            {status === "prompt" ? (
              <Button
                type="button"
                size="sm"
                disabled={busy}
                onClick={() => void enable(() => active.current)}
              >
                {t(
                  requesting
                    ? "notificationPermission.requesting"
                    : "notificationPermission.allow",
                )}
              </Button>
            ) : (
              permission?.can_open_settings && (
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={busy}
                  onClick={async () => {
                    setOpening(true);
                    setOpenFailed(false);
                    setOpened(false);
                    try {
                      await notifications.openSettings();
                      setOpened(true);
                    } catch {
                      setOpenFailed(true);
                    } finally {
                      setOpening(false);
                    }
                  }}
                >
                  {t("notificationPermission.openSettings")}
                  <ExternalLink aria-hidden="true" />
                </Button>
              )
            )}
            <Button
              type="button"
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => void refresh()}
            >
              {t(
                checking
                  ? "notificationPermission.checking"
                  : "notificationPermission.checkAgain",
              )}
            </Button>
          </div>
          {opened && (
            <p role="status" className="text-muted-foreground">
              {t("notificationPermission.returnHint")}
            </p>
          )}
          {openFailed && (
            <p role="alert" className="text-destructive">
              {t("notificationPermission.openFailed")} {instructions}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
