import { useRef, useState } from "react";
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
import { recovery } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import type { BackupCreated, BackupStatus } from "@/lib/api/recovery";

export function DataRecoverySettings() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [key, setKey] = useState("");
  const [challenge, setChallenge] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [created, setCreated] = useState<BackupCreated | null>(null);
  const [last, setLast] = useState<BackupStatus | null>(null);
  const preparation = useRef(0);
  const groups = key.split("-");
  const challengeAnswer =
    groups.length === 8 ? `${groups[1]}-${groups[6]}` : "";

  const prepare = async () => {
    const current = ++preparation.current;
    setError("");
    setCreated(null);
    setChallenge("");
    setKey("");
    try {
      const nextKey = await recovery.prepareKey();
      if (current !== preparation.current) return;
      setKey(nextKey);
      const status = await recovery.status();
      if (current === preparation.current) setLast(status);
    } catch (cause) {
      if (current === preparation.current) setError(errorMessage(cause));
    }
  };

  const create = async () => {
    if (!key || challenge.trim().toLowerCase() !== challengeAnswer) return;
    const destination = await save({
      defaultPath: `mesh-talk-${new Date().toISOString().slice(0, 10)}.mtbackup`,
      filters: [{ name: "Mesh-Talk backup", extensions: ["mtbackup"] }],
    });
    if (!destination) return;
    setBusy(true);
    setError("");
    try {
      const backup = await recovery.create(destination, key);
      setCreated(backup);
      setLast({
        destination,
        preview: backup.preview,
        archive_bytes: backup.archive_bytes,
      });
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
          if (next) void prepare();
          else {
            preparation.current++;
            setKey("");
            setChallenge("");
            setCreated(null);
          }
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
                onChange={(event) => setChallenge(event.target.value)}
                autoComplete="off"
                placeholder="XXXXXXXX-XXXXXXXX"
              />
            </div>
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
              <p className="font-medium">{t("recovery.backupComplete")}</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {t("recovery.backupSummary", {
                  count: created.preview.messages,
                  size: (created.archive_bytes / 1024 / 1024).toFixed(1),
                })}
              </p>
            </div>
          )}
          {last && (
            <p className="break-all text-xs text-muted-foreground">
              {t("recovery.lastBackup", {
                date: new Date(last.preview.created_at_ms).toLocaleString(),
                path: last.destination,
              })}
            </p>
          )}
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
                challenge.trim().toLowerCase() !== challengeAnswer
              }
              onClick={() => void create()}
            >
              {busy ? t("recovery.exporting") : t("recovery.chooseDestination")}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
