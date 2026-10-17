import { useRef, useState } from "react";
import { History } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import {
  DataRecoverySettings,
  BackupStatusSummary,
} from "../DataRecoverySettings";
import { SettingsRow, SettingsError, SELECT_CLASS } from "./primitives";
import { retentionTightens } from "./model";
import type { SettingsController } from "./useSettingsController";
export function HistorySettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const s = c.settings;
  const [pending, setPending] = useState<number | null>(null);
  const [backupOpen, setBackupOpen] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <>
      {s && (
        <SettingsRow
          id="setting-retention"
          icon={<History className="h-4 w-4" />}
          title={t("settings.retention")}
          desc={t("settings.retentionDesc")}
          control={
            <select
              id="setting-retention"
              data-testid="settings-retention-select"
              value={s.retention_days}
              disabled={c.saving}
              className={SELECT_CLASS}
              onChange={(e) => {
                const next = Number(e.target.value);
                if (retentionTightens(s.retention_days, next)) setPending(next);
                else void c.update({ retention_days: next }, "history");
              }}
            >
              <option value={0}>{t("settings.retentionForever")}</option>
              {[7, 30, 90].map((days) => (
                <option key={days} value={days}>
                  {t("settings.retentionDays", { count: days })}
                </option>
              ))}
            </select>
          }
        />
      )}
      <div className="pt-5">
        <SettingsRow
          title={t("recovery.createBackup")}
          desc={t("recovery.exportIntro")}
          control={
            <DataRecoverySettings
              open={backupOpen}
              onOpenChange={setBackupOpen}
              onBackupCreated={() => setRefresh((value) => value + 1)}
            />
          }
        />
        <BackupStatusSummary refreshToken={refresh} />
      </div>
      <Dialog
        open={pending !== null}
        onOpenChange={(open) => {
          if (!open && !c.saving) setPending(null);
        }}
      >
        <DialogContent
          className="max-h-[calc(100vh-2rem)] supports-[height:100dvh]:max-h-[calc(100dvh-2rem)] max-w-md overflow-y-auto"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            cancelRef.current?.focus();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("settings.retentionConfirmTitle")}</DialogTitle>
            <DialogDescription>
              {t("settings.retentionConfirmDesc", { count: pending })}
            </DialogDescription>
          </DialogHeader>
          <SettingsError
            error={c.errors.history ?? null}
            onRetry={c.reload}
            onDismiss={() => c.clearError("history")}
          />
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              ref={cancelRef}
              variant="secondary"
              disabled={c.saving}
              onClick={() => setPending(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              variant="outline"
              disabled={c.saving}
              onClick={() => {
                setPending(null);
                setBackupOpen(true);
              }}
            >
              {t("settings.retentionBackupFirst")}
            </Button>
            <Button
              variant="destructive"
              disabled={c.saving || pending === null || !s}
              onClick={async () => {
                if (
                  pending !== null &&
                  (await c.update({ retention_days: pending }, "history"))
                )
                  setPending(null);
              }}
            >
              {c.saving ? t("settings.saving") : t("settings.retentionConfirm")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
