import { useCallback, useEffect, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
import type { BackupCreated, BackupStatus } from "@/lib/api/recovery";

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
}: {
  enabled?: boolean;
  refreshToken?: number;
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
      <p>{t("recovery.restoreHint")}</p>
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
  const [key, setKey] = useState("");
  const [challenge, setChallenge] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [destinationKind, setDestinationKind] = useState("local");
  const [connectionId, setConnectionId] = useState(() => crypto.randomUUID());
  const [driveConnected, setDriveConnected] = useState(false);
  const [created, setCreated] = useState<BackupCreated | null>(null);
  const pending = useRef(false);
  useEffect(
    () => () => {
      preparation.current++;
    },
    [],
  );
  const preparation = useRef(0);
  const groups = key.split("-");
  const challengeAnswer =
    groups.length === 8 ? `${groups[1]}-${groups[6]}` : "";

  const prepare = useCallback(async () => {
    if (pending.current) return;
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    const current = ++preparation.current;
    setError("");
    setCreated(null);
    setChallenge("");
    setKey("");
    try {
      const nextKey = await recovery.prepareKey();
      if (current !== preparation.current || !lease.current()) return;
      setKey(nextKey);
    } catch (cause) {
      if (current === preparation.current && lease.current())
        setError(errorMessage(cause));
    }
  }, []);

  useEffect(() => {
    preparation.current++;
    pending.current = false;
    setBusy(false);
    setKey("");
    setChallenge("");
    setCreated(null);
    setError("");
    setDriveConnected(false);
    if (open) void prepare();
  }, [open, prepare]);

  const create = async () => {
    if (
      pending.current ||
      created ||
      !key ||
      challenge.trim().toLowerCase() !== challengeAnswer
    )
      return;
    if (!captureChatOwnership().current()) return;
    // Snapshot creation intentionally pauses the node. A temporary readiness
    // change must not hide completion or strand the dialog in its busy state.
    const lease = captureRuntimeOwner();
    const generation = preparation.current;
    const current = () => generation === preparation.current && lease.current();
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      let backup: BackupCreated;
      if (destinationKind === "drive") {
        if (!driveConnected) return;
        backup = await googleDrive.create(connectionId, key);
      } else {
        const destination = await save({
          defaultPath: `mesh-talk-${new Date().toISOString().slice(0, 10)}.mtbackup`,
          filters: [{ name: "Mesh-Talk backup", extensions: ["mtbackup"] }],
        });
        if (!destination || !current()) return;
        backup = await recovery.create(destination, key);
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
      }
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!pending.current) {
          setOpen(next);
          onOpenChange?.(next);
        }
      }}
    >
      <DialogTrigger asChild>
        <Button variant="secondary" size="sm">
          {t("recovery.createBackup")}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[min(42rem,calc(100vh-2rem))] max-w-lg overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("recovery.createBackup")}</DialogTitle>
          <DialogDescription>{t("recovery.exportIntro")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4 text-sm">
          <div className="rounded-md border bg-muted/35 p-3 text-muted-foreground">
            <p>{t("recovery.included")}</p>
            <p className="mt-2">{t("recovery.excluded")}</p>
          </div>
          <p className="text-muted-foreground">{t("recovery.pauseNotice")}</p>
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
              disabled={busy || !!created}
              onChange={(event) => {
                setDestinationKind(event.target.value);
                setDriveConnected(false);
              }}
            >
              <option value="local">{t("drive.local")}</option>
              <option value="drive">{t("drive.cloud")}</option>
            </select>
            {open && destinationKind === "drive" && (
              <GoogleDriveConnection
                key={connectionId}
                connectionId={connectionId}
                disabled={busy}
                onConnected={() => setDriveConnected(true)}
              />
            )}
          </div>
          {key && (
            <div className="space-y-2">
              <p className="font-medium">{t("recovery.saveKey")}</p>
              <code
                data-testid="recovery-export-key"
                className="block select-all break-all rounded-md border bg-background p-3 font-mono text-xs leading-6"
              >
                {key}
              </code>
              <p className="text-xs text-muted-foreground">
                {t("recovery.perExportKey")}
              </p>
              <label
                className="block text-xs font-medium"
                htmlFor="recovery-key-check"
              >
                {t("recovery.confirmGroups")}
              </label>
              <Input
                id="recovery-key-check"
                data-testid="recovery-key-check"
                value={challenge}
                disabled={busy || !!created}
                onChange={(event) => setChallenge(event.target.value)}
                autoComplete="off"
                placeholder="XXXXXXXX-XXXXXXXX"
              />
            </div>
          )}
          {busy && destinationKind === "drive" && (
            <>
              <GoogleDriveProgress connectionId={connectionId} />
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setDriveConnected(false);
                  void googleDrive
                    .disconnect(connectionId)
                    .catch((cause) => setError(errorMessage(cause)))
                    .finally(() => setConnectionId(crypto.randomUUID()));
                }}
              >
                {t("drive.cancel")}
              </Button>
            </>
          )}
          {!busy && driveConnected && !created && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setDriveConnected(false);
                setConnectionId(crypto.randomUUID());
              }}
            >
              {t("drive.reconnect")}
            </Button>
          )}
          {error && (
            <p role="alert" className="text-destructive">
              {error}
            </p>
          )}
          {created && (
            <div
              role="status"
              className="rounded-md border border-signal/40 bg-signal/5 p-3"
            >
              <p className="font-medium">
                {destinationKind === "drive"
                  ? t("drive.complete")
                  : t("recovery.backupComplete")}
              </p>
              <p className="mt-1 text-xs text-muted-foreground">
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
          />
          <div className="flex justify-end gap-2">
            <Button
              variant="secondary"
              onClick={() => void prepare()}
              disabled={busy}
            >
              {t("recovery.newKey")}
            </Button>
            <Button
              data-testid="recovery-export"
              disabled={
                busy ||
                !key ||
                !!created ||
                (destinationKind === "drive" && !driveConnected) ||
                challenge.trim().toLowerCase() !== challengeAnswer
              }
              onClick={() => void create()}
            >
              {busy
                ? t("recovery.exporting")
                : destinationKind === "drive"
                  ? t("drive.backUpNow")
                  : t("recovery.chooseDestination")}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
