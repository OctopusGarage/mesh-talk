import { useState } from "react";
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
  const [path, setPath] = useState("");
  const [key, setKey] = useState("");
  const [preview, setPreview] = useState<BackupPreview | null>(null);
  const [result, setResult] = useState<RestoreComplete | null>(null);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [compromised, setCompromised] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

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
    setBusy(true);
    setError("");
    try {
      setPreview(await recovery.inspect(path, key));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  };
  const restore = async () => {
    setBusy(true);
    setError("");
    try {
      const restored = await recovery.restore(
        path,
        key,
        username.trim(),
        password,
      );
      setResult(restored);
      setPassword("");
      setKey("");
      onRestored(restored.username);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
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
                <Label>{t("recovery.backupFile")}</Label>
                <Button
                  variant="secondary"
                  onClick={() => void choose()}
                  disabled={busy}
                >
                  {path ? path.split(/[\\/]/).pop() : t("recovery.chooseFile")}
                </Button>
              </div>
              <div className="space-y-2">
                <Label htmlFor="restore-key">{t("recovery.recoveryKey")}</Label>
                <Input
                  id="restore-key"
                  data-testid="restore-key"
                  type="password"
                  autoComplete="off"
                  value={key}
                  onChange={(event) => {
                    setKey(event.target.value);
                    setPreview(null);
                  }}
                />
              </div>
              {!preview && (
                <Button
                  data-testid="restore-inspect"
                  disabled={!path || !key || busy}
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
