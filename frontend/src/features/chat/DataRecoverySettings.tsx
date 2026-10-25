import { useCallback, useEffect, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { BackupRecoveryKey } from "@/features/recovery/BackupRecoveryKey";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { useAuth } from "@/store/auth";
import { useChat, captureChatOwnership } from "@/store/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import { recovery } from "@/lib/api";
import { googleDrive } from "@/lib/api/googleDrive";
import { GoogleDriveConnection } from "@/features/recovery/GoogleDriveConnection";
import { GoogleDriveProgress } from "@/features/recovery/GoogleDriveProgress";
import { errorMessage } from "@/lib/error";
import type {
  BackupCreated,
  BackupStatus,
  BackupSetup,
  BackupDestination,
} from "@/lib/api/recovery";

export function DataRecoverySettings({
  enabled = true,
  onBackupCreated,
  open,
  onOpenChange,
}: {
  enabled?: boolean;
  onBackupCreated?: () => void;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const owner = useAuth((state) => state.generation);
  const run = useChat((state) => state.runEpoch);
  const identity = useChat((state) => state.identityEpoch);
  return enabled ? (
    <BackupDialog
      key={`${owner}:${run}:${identity}`}
      onBackupCreated={onBackupCreated}
      controlledOpen={open}
      onOpenChange={onOpenChange}
    />
  ) : null;
}

export function BackupStatusSummary({
  enabled = true,
  refreshToken = 0,
  showRestoreHint = true,
}: {
  enabled?: boolean;
  refreshToken?: number;
  showRestoreHint?: boolean;
}) {
  const { t } = useTranslation();
  const owner = useAuth((state) => state.generation);
  const run = useChat((state) => state.runEpoch);
  const identity = useChat((state) => state.identityEpoch);
  const [status, setStatus] = useState<BackupStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    let current = true;
    const lease = captureRuntimeOwner();
    setStatus(null);
    setLoading(true);
    setFailed(null);
    if (enabled)
      void recovery.status().then(
        (next) => {
          if (current && lease.current()) {
            setStatus(next);
            setLoading(false);
          }
        },
        (reason) => {
          if (current && lease.current()) {
            setFailed(errorMessage(reason));
            setLoading(false);
          }
        },
      );
    return () => {
      current = false;
    };
  }, [enabled, owner, run, identity, retry, refreshToken]);
  return (
    <div
      className="space-y-2 text-xs text-muted-foreground"
      data-testid="backup-status-summary"
    >
      <p className="break-all" role={failed ? "alert" : "status"}>
        {loading
          ? t("common.loading")
          : failed
            ? t("recovery.statusUnavailable", { error: failed })
            : status
              ? t("recovery.lastBackup", {
                  date: new Date(status.preview.created_at_ms).toLocaleString(),
                  path: status.destination,
                })
              : t("recovery.noBackup")}
      </p>
      {failed && (
        <Button
          variant="secondary"
          size="sm"
          onClick={() => setRetry((value) => value + 1)}
        >
          {t("contactVisibility.retry")}
        </Button>
      )}
      {showRestoreHint && <p>{t("recovery.restoreHint")}</p>}
    </div>
  );
}

function BackupDialog({
  onBackupCreated,
  controlledOpen,
  onOpenChange,
}: {
  onBackupCreated?: () => void;
  controlledOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const [localOpen, setOpen] = useState(false);
  const open = controlledOpen ?? localOpen;
  const [setup, setSetup] = useState<BackupSetup | null>(null);
  const [savedKey, setSavedKey] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [savingDestination, setSavingDestination] = useState(false);
  const [error, setError] = useState("");
  const [destinationKind, setDestinationKind] =
    useState<BackupDestination>("local");
  const [connectionId, setConnectionId] = useState(() => crypto.randomUUID());
  const [driveConnected, setDriveConnected] = useState(false);
  const [created, setCreated] = useState<BackupCreated | null>(null);
  const pending = useRef(false);
  const cancelling = useRef(false);
  const preparation = useRef(0);
  useEffect(
    () => () => {
      preparation.current++;
    },
    [],
  );

  const prepare = useCallback(async () => {
    if (pending.current) return;
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    const generation = ++preparation.current;
    setError("");
    setLoading(true);
    try {
      const next = await recovery.setup();
      if (generation !== preparation.current || !lease.current()) return;
      setSetup(next);
      setDestinationKind(next.destination);
    } catch (cause) {
      if (generation === preparation.current && lease.current())
        setError(errorMessage(cause));
    } finally {
      if (generation === preparation.current && lease.current())
        setLoading(false);
    }
  }, []);

  useEffect(() => {
    preparation.current++;
    pending.current = false;
    setBusy(false);
    setSavingDestination(false);
    setSetup(null);
    setSavedKey(false);
    setCreated(null);
    setError("");
    setDriveConnected(false);
    setConnectionId(crypto.randomUUID());
    if (open) void prepare();
  }, [open, prepare]);

  const changeOpen = (next: boolean) => {
    if (pending.current) return;
    setOpen(next);
    onOpenChange?.(next);
  };

  const changeDestination = async (next: BackupDestination) => {
    if (pending.current) return;
    const lease = captureRuntimeOwner();
    const generation = preparation.current;
    const current = () => generation === preparation.current && lease.current();
    pending.current = true;
    setSavingDestination(true);
    setError("");
    try {
      await recovery.setDestination(next);
      if (!current()) return;
      setDestinationKind(next);
      setDriveConnected(false);
      setConnectionId(crypto.randomUUID());
    } catch (cause) {
      if (current()) setError(errorMessage(cause));
    } finally {
      if (current()) {
        pending.current = false;
        setSavingDestination(false);
      }
    }
  };

  const create = async () => {
    if (pending.current || created || !setup || (!setup.confirmed && !savedKey))
      return;
    if (destinationKind === "drive" && !driveConnected) return;
    if (!captureChatOwnership().current()) return;
    // Snapshot creation pauses messaging; completion belongs to the runtime owner,
    // independent of temporary node readiness during the restart.
    const lease = captureRuntimeOwner();
    const generation = preparation.current;
    const current = () => generation === preparation.current && lease.current();
    pending.current = true;
    cancelling.current = false;
    setBusy(true);
    setError("");
    try {
      await recovery.setDestination(destinationKind);
      if (!current()) return;
      if (!setup.confirmed) {
        await recovery.confirmKey(setup.recovery_key);
        if (!current()) return;
        setSetup({ ...setup, confirmed: true });
      }
      let backup: BackupCreated;
      if (destinationKind === "drive") {
        backup = await googleDrive.create(connectionId);
      } else {
        const destination = await save({
          defaultPath: `mesh-talk-${new Date().toISOString().replace(/[:.]/g, "-")}.mtbackup`,
          filters: [{ name: "Mesh-Talk backup", extensions: ["mtbackup"] }],
        });
        if (!destination || !current()) return;
        backup = await recovery.create(destination);
      }
      if (current()) {
        setCreated(backup);
        onBackupCreated?.();
      }
    } catch (cause) {
      if (current()) setError(errorMessage(cause));
    } finally {
      if (current()) {
        pending.current = false;
        setBusy(false);
        if (cancelling.current) {
          setDriveConnected(false);
          setConnectionId(crypto.randomUUID());
        }
      }
    }
  };

  return (
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogTrigger asChild>
        <Button variant="secondary" size="sm">
          {t("recovery.createBackup")}
        </Button>
      </DialogTrigger>
      <DialogContent className="flex max-h-[min(42rem,calc(100vh-2rem))] max-w-lg flex-col gap-0 overflow-hidden p-0">
        <DialogHeader className="shrink-0 p-6 pb-4">
          <DialogTitle>{t("recovery.createBackup")}</DialogTitle>
          <DialogDescription>{t("recovery.simpleIntro")}</DialogDescription>
        </DialogHeader>
        <div className="min-h-0 space-y-5 overflow-y-auto px-6 pb-4 text-sm">
          {!created && (
            <>
              <div className="space-y-2">
                <label
                  className="block text-xs font-medium"
                  htmlFor="backup-destination"
                >
                  {t("drive.destination")}
                </label>
                <select
                  id="backup-destination"
                  className="w-full rounded-md border bg-background p-2"
                  value={destinationKind}
                  disabled={busy || savingDestination || loading || !setup}
                  onChange={(event) =>
                    void changeDestination(
                      event.target.value as BackupDestination,
                    )
                  }
                >
                  <option value="local">{t("drive.local")}</option>
                  <option value="drive">{t("drive.cloud")}</option>
                </select>
                {open && setup && destinationKind === "drive" && (
                  <GoogleDriveConnection
                    key={connectionId}
                    connectionId={connectionId}
                    disabled={busy || savingDestination}
                    onConnected={() => setDriveConnected(true)}
                    onDisconnected={() => setDriveConnected(false)}
                  />
                )}
              </div>
              {loading && (
                <p role="status" className="text-muted-foreground">
                  {t("common.loading")}
                </p>
              )}
              {setup && (destinationKind === "local" || driveConnected) && (
                <BackupRecoveryKey
                  recoveryKey={setup.recovery_key}
                  confirmed={setup.confirmed}
                  disabled={busy || savingDestination}
                />
              )}
              <details className="text-xs text-muted-foreground">
                <summary className="cursor-pointer py-1 font-medium text-foreground">
                  {t("recovery.backupDetails")}
                </summary>
                <div className="mt-2 space-y-2 leading-relaxed">
                  <p>{t("recovery.included")}</p>
                  <p>{t("recovery.excluded")}</p>
                  <p>{t("recovery.pauseNotice")}</p>
                </div>
              </details>
            </>
          )}
          {busy && destinationKind === "drive" && (
            <div className="space-y-2">
              <GoogleDriveProgress connectionId={connectionId} />
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  const lease = captureRuntimeOwner();
                  const generation = preparation.current;
                  cancelling.current = true;
                  void googleDrive.disconnect(connectionId).catch((cause) => {
                    if (generation === preparation.current && lease.current())
                      setError(errorMessage(cause));
                  });
                  setDriveConnected(false);
                  // Keep the operation owned until the snapshot safely restarts messaging.
                }}
              >
                {t("drive.cancel")}
              </Button>
            </div>
          )}
          {error && (
            <p role="alert" className="break-words text-destructive">
              {error}
            </p>
          )}
          {!loading && !setup && error && (
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void prepare()}
            >
              {t("contactVisibility.retry")}
            </Button>
          )}
          {created && (
            <div role="status" className="space-y-2 py-2">
              <p className="font-medium">
                {destinationKind === "drive"
                  ? t("drive.complete")
                  : t("recovery.backupComplete")}
              </p>
              <p className="text-xs text-muted-foreground">
                {t("recovery.backupSummary", {
                  count: created.preview.messages,
                  size: (created.archive_bytes / 1024 / 1024).toFixed(1),
                })}
              </p>
            </div>
          )}
          <BackupStatusSummary
            key={created ? "created" : "initial"}
            enabled={open}
            showRestoreHint={false}
          />
        </div>
        <div className="shrink-0 space-y-3 border-t bg-background px-6 py-4">
          {setup &&
            !setup.confirmed &&
            !created &&
            (destinationKind === "local" || driveConnected) && (
              <label className="flex items-start gap-2 text-sm leading-relaxed">
                <input
                  data-testid="recovery-key-saved"
                  type="checkbox"
                  checked={savedKey}
                  disabled={busy || savingDestination}
                  onChange={(event) => setSavedKey(event.target.checked)}
                  className="mt-1 accent-signal"
                />
                {t("recovery.keySaved")}
              </label>
            )}
          <div className="flex justify-end gap-2">
            {created ? (
              <Button onClick={() => changeOpen(false)}>
                {t("common.done")}
              </Button>
            ) : (
              <Button
                data-testid="recovery-export"
                disabled={
                  busy ||
                  savingDestination ||
                  loading ||
                  !setup ||
                  (destinationKind === "drive" && !driveConnected) ||
                  (!setup.confirmed && !savedKey)
                }
                onClick={() => void create()}
              >
                {busy ? t("recovery.exporting") : t("drive.backUpNow")}
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
