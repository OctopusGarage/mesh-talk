import {
  cloneElement,
  isValidElement,
  useId,
  type ReactElement,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import type { SettingsController } from "./useSettingsController";
import type { InactiveReason } from "./model";

export const SELECT_CLASS =
  "h-9 min-w-0 max-w-full rounded-md border border-input bg-background px-2 text-[13px] transition-[border-color,box-shadow] duration-150 hover:border-ring/40 focus-visible:border-ring focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring/25 [@media(hover:none)]:min-h-11";

export function SettingsSection({
  title,
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <section className="space-y-2">
      {title && (
        <h3 className="text-[12px] font-semibold text-muted-foreground">
          {title}
        </h3>
      )}
      <div>{children}</div>
    </section>
  );
}

/** Label and help share one association with every ordinary switch/select editor. */
export function SettingsRow({
  id,
  icon,
  title,
  desc,
  control,
  reason,
}: {
  id?: string;
  icon?: ReactNode;
  title: string;
  desc: ReactNode;
  control: ReactNode;
  reason?: ReactNode;
}) {
  const generated = useId();
  const helpId = `${id ?? generated}-help`;
  const editor = isValidElement(control)
    ? cloneElement(control as ReactElement<{ "aria-describedby"?: string }>, {
        "aria-describedby": helpId,
      })
    : control;
  return (
    <div className="flex min-w-0 items-center justify-between gap-4 border-b py-3 last:border-b-0 max-[700px]:flex-col max-[700px]:items-stretch max-[700px]:gap-2">
      <div className="flex min-w-0 flex-1 items-start gap-3">
        {icon && (
          <div className="mt-0.5 shrink-0 text-muted-foreground">{icon}</div>
        )}
        <div className="min-w-0">
          <label htmlFor={id} className="block text-[13px] font-medium">
            {title}
          </label>
          <div
            id={helpId}
            className="text-[12px] leading-[1.5] text-muted-foreground"
          >
            {desc}
            {reason && <p className="mt-1 font-medium">{reason}</p>}
          </div>
        </div>
      </div>
      <div className="min-w-0 max-w-full shrink-0 max-[700px]:self-start">
        {editor}
      </div>
    </div>
  );
}

export function InactiveHelp({ reason }: { reason: InactiveReason | null }) {
  const { t } = useTranslation();
  if (!reason) return null;
  const key = {
    loading: "common.loading",
    saving: "settings.saving",
    relayOff: "settings.relayDisabledReason",
    physicalOnly: "settings.physicalOnlyReason",
    invisible: "settings.invisibleReason",
    privacyUnknown: "settings.privacyUnknownReason",
  }[reason];
  return <span>{t(key)}</span>;
}

export function SettingsError({
  error,
  onRetry,
  onDismiss,
}: {
  error: SettingsController["loadError"];
  onRetry: () => void;
  onDismiss?: () => void;
}) {
  const { t } = useTranslation();
  if (!error) return null;
  return (
    <div
      role="alert"
      data-testid="settings-error"
      className="flex items-start justify-between gap-3 rounded-md border border-destructive/35 bg-destructive/5 px-3 py-2 text-sm text-destructive"
    >
      <span className="min-w-0 break-words">
        {t(
          error.kind === "refresh"
            ? "settings.refreshFailed"
            : error.kind === "load"
              ? "settings.loadFailed"
              : "settings.saveFailed",
          { error: error.detail },
        )}
      </span>
      <button
        type="button"
        onClick={error.kind === "save" && onDismiss ? onDismiss : onRetry}
        className="shrink-0 rounded-sm font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {t(
          error.kind === "save" && onDismiss
            ? "common.dismiss"
            : error.kind === "refresh"
              ? "settings.reload"
              : "contactVisibility.retry",
        )}
      </button>
    </div>
  );
}
