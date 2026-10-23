import { useEffect, useRef, useState } from "react";
import { open as openFile } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { recovery } from "@/lib/api";
import { googleDrive, type DriveArchive } from "@/lib/api/googleDrive";
import { GoogleDriveConnection } from "@/features/recovery/GoogleDriveConnection";
import { GoogleDriveProgress } from "@/features/recovery/GoogleDriveProgress";
import { useAuth } from "@/store/auth";
import type { BackupPreview, RestoreComplete } from "@/lib/api/recovery";
import { errorMessage } from "@/lib/error";

function dateLabel(ms: number) {
  return new Date(ms).toLocaleString();
}

export function RestoreBackup({
  onRestored,
  onStartFresh,
}: {
  onRestored: (username: string) => void;
  onStartFresh: () => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [source, setSource] = useState("local");
  const [connectionId, setConnectionId] = useState(() => crypto.randomUUID());
  const [driveConnected, setDriveConnected] = useState(false);
  const [archives, setArchives] = useState<DriveArchive[]>([]);
  const [selectedArchive, setSelectedArchive] = useState("");
  const [downloaded, setDownloaded] = useState(false);
  const [transfer, setTransfer] = useState(false);
  const generation = useRef(0);
  const authGeneration = useAuth((state) => state.generation);
  useEffect(() => {
    const run = ++generation.current;
    return () => {
      generation.current = run + 1;
    };
  }, [authGeneration, open, source, connectionId]);
  const [path, setPath] = useState("");
  const [key, setKey] = useState("");
  const [preview, setPreview] = useState<BackupPreview | null>(null);
  const [result, setResult] = useState<RestoreComplete | null>(null);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [compromised, setCompromised] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const loadBackups = async () => {
    const run = generation.current;
    setBusy(true);
    setError("");
    try {
      const list = await googleDrive.list(connectionId);
      if (run === generation.current) {
        setArchives(list);
        setSelectedArchive(list[0]?.id ?? "");
      }
    } catch (cause) {
      if (run === generation.current) setError(errorMessage(cause));
    } finally {
      if (run === generation.current) setBusy(false);
    }
  };
  const download = async () => {
    const run = generation.current;
    setBusy(true);
    setTransfer(true);
    setError("");
    setPreview(null);
    setDownloaded(false);
    setKey("");
    try {
      await googleDrive.download(connectionId, selectedArchive);
      if (run === generation.current) setDownloaded(true);
    } catch (cause) {
      if (run === generation.current) setError(errorMessage(cause));
    } finally {
      if (run === generation.current) {
        setBusy(false);
        setTransfer(false);
      }
    }
  };
  const choose = async () => {
    const selected = await openFile({
      multiple: false,
      filters: [{ name: "Mesh-Talk backup", extensions: ["mtbackup"] }],
    });
    if (typeof selected === "string") {
      setPath(selected);
      setKey("");
      setPreview(null);
      setResult(null);
      setError("");
    }
  };
  const inspect = async () => {
    const run = generation.current;
    setBusy(true);
    setError("");
    try {
      const next =
        source === "drive"
          ? await googleDrive.inspect(connectionId, key)
          : await recovery.inspect(path, key);
      if (run === generation.current) setPreview(next);
    } catch (cause) {
      if (run === generation.current) {
        setError(errorMessage(cause));
        setPreview(null);
      }
    } finally {
      if (run === generation.current) setBusy(false);
    }
  };
  const restore = async () => {
    const run = generation.current;
    setBusy(true);
    setError("");
    try {
      const restored =
        source === "drive"
          ? await googleDrive.restore(
              connectionId,
              key,
              username.trim(),
              password,
            )
          : await recovery.restore(path, key, username.trim(), password);
      if (run !== generation.current) return;
      setResult(restored);
      setPassword("");
      setKey("");
      onRestored(restored.username);
    } catch (cause) {
      if (run === generation.current) setError(errorMessage(cause));
    } finally {
      if (run === generation.current) setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!busy) {
          setOpen(next);
          if (!next) {
            setKey("");
            setPassword("");
            setPreview(null);
            setResult(null);
            setDownloaded(false);
            setDriveConnected(false);
            setArchives([]);
            setSelectedArchive("");
            setConnectionId(crypto.randomUUID());
          }
        }
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline" className="w-full">
          {t("recovery.restoreBackup")}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[min(43rem,calc(100vh-2rem))] max-w-lg overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("recovery.restoreBackup")}</DialogTitle>
          <DialogDescription>{t("recovery.restoreIntro")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4 text-sm">
          {!result && (
            <>
              <div className="space-y-2">
                <Label htmlFor="backup-source">{t("drive.source")}</Label>
                <select
                  id="backup-source"
                  className="w-full rounded-md border bg-background p-2"
                  disabled={busy}
                  value={source}
                  onChange={(event) => {
                    setSource(event.target.value);
                    setPreview(null);
                    setKey("");
                    setDownloaded(false);
                    setError("");
                    setDriveConnected(false);
                    setArchives([]);
                    setSelectedArchive("");
                  }}
                >
                  <option value="local">{t("drive.local")}</option>
                  <option value="drive">{t("drive.cloud")}</option>
                </select>
              </div>
              {open && source === "drive" && (
                <div className="space-y-2">
                  <GoogleDriveConnection
                    key={connectionId}
                    connectionId={connectionId}
                    disabled={busy}
                    onConnected={() => {
                      setDriveConnected(true);
                      void loadBackups();
                    }}
                  />
                  {driveConnected && (
                    <>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy}
                        onClick={() => void loadBackups()}
                      >
                        {busy && !transfer
                          ? t("drive.loading")
                          : t("drive.load")}
                      </Button>
                      {archives.length > 0 ? (
                        <>
                          <Label htmlFor="drive-backup">
                            {t("drive.choose")}
                          </Label>
                          <select
                            id="drive-backup"
                            className="w-full rounded-md border bg-background p-2"
                            value={selectedArchive}
                            disabled={busy}
                            onChange={(event) => {
                              setSelectedArchive(event.target.value);
                              setDownloaded(false);
                              setPreview(null);
                              setKey("");
                            }}
                          >
                            {archives.map((item) => (
                              <option key={item.id} value={item.id}>
                                {item.name} (
                                {(Number(item.size) / 1024 / 1024).toFixed(1)}{" "}
                                MB)
                              </option>
                            ))}
                          </select>
                          <Button
                            variant="secondary"
                            disabled={busy || !selectedArchive}
                            onClick={() => void download()}
                          >
                            {t("drive.download")}
                          </Button>
                        </>
                      ) : (
                        !busy && <p role="status">{t("drive.empty")}</p>
                      )}
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy}
                        onClick={() => {
                          setDriveConnected(false);
                          setDownloaded(false);
                          setPreview(null);
                          setKey("");
                          setArchives([]);
                          setSelectedArchive("");
                          setConnectionId(crypto.randomUUID());
                        }}
                      >
                        {t("drive.reconnect")}
                      </Button>
                    </>
                  )}
                  {downloaded && <p role="status">{t("drive.downloaded")}</p>}
                  {transfer && (
                    <>
                      <GoogleDriveProgress connectionId={connectionId} />
                      <Button
                        variant="outline"
                        onClick={() => {
                          void googleDrive
                            .disconnect(connectionId)
                            .catch(() => {});
                          generation.current++;
                          setBusy(false);
                          setTransfer(false);
                          setDriveConnected(false);
                          setDownloaded(false);
                          setArchives([]);
                          setConnectionId(crypto.randomUUID());
                        }}
                      >
                        {t("drive.cancel")}
                      </Button>
                    </>
                  )}
                </div>
              )}
              {source === "local" && (
                <div className="space-y-2">
                  <Label>{t("recovery.backupFile")}</Label>
                  <Button
                    variant="secondary"
                    onClick={() => void choose()}
                    disabled={busy}
                  >
                    {path
                      ? path.split(/[\\/]/).pop()
                      : t("recovery.chooseFile")}
                  </Button>
                </div>
              )}
              <div className="space-y-2">
                <Label htmlFor="restore-key">{t("recovery.recoveryKey")}</Label>
                <Input
                  id="restore-key"
                  data-testid="restore-key"
                  type="password"
                  autoComplete="off"
                  value={key}
                  disabled={busy}
                  onChange={(event) => {
                    setKey(event.target.value);
                    setPreview(null);
                  }}
                />
              </div>
              {!preview && (
                <Button
                  data-testid="restore-inspect"
                  disabled={
                    (source === "drive" ? !downloaded : !path) || !key || busy
                  }
                  onClick={() => void inspect()}
                >
                  {busy ? t("recovery.verifying") : t("recovery.inspect")}
                </Button>
              )}
            </>
          )}
          {preview && !result && (
            <>
              <div
                data-testid="restore-preview"
                className="rounded-md border bg-muted/35 p-3 space-y-1"
              >
                <p className="font-medium">{t("recovery.previewTitle")}</p>
                <p>
                  {t("recovery.snapshotDate", {
                    date: dateLabel(preview.created_at_ms),
                  })}
                </p>
                <p className="break-all font-mono text-xs">
                  {preview.account_id}
                </p>
                <p>
                  {t("recovery.previewCounts", {
                    messages: preview.messages,
                    channels: preview.channels,
                    files: preview.file_cards,
                  })}
                </p>
                <p>
                  {t("recovery.missingMedia", { count: preview.missing_media })}
                </p>
                <p className="text-xs text-muted-foreground">
                  {t("recovery.newerMissing")}
                </p>
              </div>
              <label className="flex items-start gap-2 text-muted-foreground">
                <input
                  type="checkbox"
                  checked={compromised}
                  disabled={busy}
                  onChange={(event) => setCompromised(event.target.checked)}
                  className="mt-1 accent-signal"
                />
                {t("recovery.oldDeviceRisk")}
              </label>
              {compromised ? (
                <div
                  role="alert"
                  className="rounded-md border border-amber-500/40 bg-amber-500/5 p-3"
                >
                  <p>{t("recovery.compromiseWarning")}</p>
                  <Button
                    className="mt-3"
                    disabled={busy}
                    variant="secondary"
                    onClick={() => {
                      setOpen(false);
                      setKey("");
                      setPassword("");
                      setPreview(null);
                      setResult(null);
                      onStartFresh();
                    }}
                  >
                    {t("recovery.startFresh")}
                  </Button>
                </div>
              ) : (
                <>
                  <div className="space-y-2">
                    <Label htmlFor="restore-username">
                      {t("login.username")}
                    </Label>
                    <Input
                      id="restore-username"
                      data-testid="restore-username"
                      autoComplete="username"
                      value={username}
                      disabled={busy}
                      onChange={(event) => setUsername(event.target.value)}
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="restore-password">
                      {t("recovery.newPassword")}
                    </Label>
                    <Input
                      id="restore-password"
                      data-testid="restore-password"
                      type="password"
                      autoComplete="new-password"
                      value={password}
                      disabled={busy}
                      onChange={(event) => setPassword(event.target.value)}
                    />
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {t("recovery.newDeviceNote")}
                  </p>
                  <Button
                    data-testid="restore-commit"
                    disabled={busy || !username.trim() || password.length < 8}
                    onClick={() => void restore()}
                  >
                    {busy ? t("recovery.restoring") : t("recovery.restoreNow")}
                  </Button>
                </>
              )}
            </>
          )}
          {result && (
            <div
              role="status"
              className="space-y-2 rounded-md border border-signal/40 bg-signal/5 p-3"
            >
              <p className="font-medium">{t("recovery.restoreComplete")}</p>
              <p className="text-xs">
                {t("recovery.accountFingerprint")}:{" "}
                <span className="break-all font-mono">
                  {result.preview.account_id}
                </span>
              </p>
              <p className="text-xs">
                {t("recovery.deviceFingerprint")}:{" "}
                <span className="break-all font-mono">{result.device_id}</span>
              </p>
              <p className="text-xs">
                {t("recovery.previewCounts", {
                  messages: result.preview.messages,
                  channels: result.preview.channels,
                  files: result.preview.file_cards,
                })}
              </p>
              <Button onClick={() => setOpen(false)}>
                {t("recovery.signInNext")}
              </Button>
            </div>
          )}
          {error && (
            <p role="alert" className="text-destructive">
              {error}
            </p>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
