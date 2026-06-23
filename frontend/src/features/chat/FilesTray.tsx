import { useEffect, useState } from "react";
import { open as openDialog, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Download, FolderOpen, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { IdentityGlyph } from "@/components/identity";
import { chat, settings as settingsApi } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { humanSize } from "@/lib/format";
import { useChat } from "@/store/chat";
import { TransferBar } from "./TransferBar";
import { fileGlyph } from "./mediaFile";

/** localStorage key for the persisted file_conv → saved-path map (where downloads landed). */
const DOWNLOADS_KEY = "mesh-talk-downloads";
function loadSavedPaths(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(DOWNLOADS_KEY) || "{}");
  } catch {
    return {};
  }
}

export function FilesTray() {
  const { t } = useTranslation();
  // "Received files" is ATTACHMENTS only. A file sent via the media button is shown inline +
  // auto-saved, so it never appears here — decided by the sender's INTENT (manifest kind),
  // not the file extension (a .mov sent via the attach button DOES belong here).
  const incoming = useChat((s) => s.incomingFiles);
  const files = incoming.filter((f) => !f.media);
  const dismissFile = useChat((s) => s.dismissFile);
  const setError = useChat((s) => s.setError);

  // Remembered default download folder ("" = always prompt). Loaded once on mount, kept as
  // a primitive (memory: zustand selectors returning fresh objects can black-screen).
  const [downloadDir, setDownloadDir] = useState("");
  useEffect(() => {
    void settingsApi.get().then(
      (s) => setDownloadDir(s.download_dir),
      () => {},
    );
  }, []);

  // Pick (and persist) the remembered default download folder.
  const chooseDir = async () => {
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (typeof dir === "string") {
        const cur = await settingsApi.get();
        await settingsApi.set({ ...cur, download_dir: dir });
        setDownloadDir(dir);
      }
    } catch (e) {
      setError(t("files.couldntSave", { error: errorMessage(e) }));
    }
  };

  // Where each saved file landed (by fileConv), so a saved row can reveal/open it instead
  // of offering Save again. PERSISTED to localStorage (keyed by the globally-unique
  // file_conv) so it survives reload/restart — otherwise a re-save would hit the pruned
  // chunks of an already-downloaded file and error ("file incomplete"). The row stays in
  // the tray until the user dismisses it.
  const [savedPaths, setSavedPaths] =
    useState<Record<string, string>>(loadSavedPaths);
  // Remember (and persist) where a file was saved.
  const remember = (fileConv: string, path: string) =>
    setSavedPaths((m) => {
      const next = { ...m, [fileConv]: path };
      try {
        localStorage.setItem(DOWNLOADS_KEY, JSON.stringify(next));
      } catch {
        // best-effort: a quota/serialization failure just loses the persisted hint.
      }
      return next;
    });

  // Save into the remembered folder (no prompt); falls back to a Save-as dialog when no
  // default is set.
  const saveToDefault = async (fileConv: string, name: string) => {
    try {
      if (downloadDir) {
        const path = await chat.saveFileToDir(fileConv, downloadDir);
        remember(fileConv, path);
        return;
      }
      const dest = await save({ defaultPath: name });
      if (typeof dest === "string") {
        await chat.saveFile(fileConv, dest);
        remember(fileConv, dest);
      }
    } catch (e) {
      handleSaveError(e);
    }
  };

  // Always-prompt "Save as…" override (ignores the remembered default).
  const saveAs = async (fileConv: string, name: string) => {
    try {
      const dest = await save({ defaultPath: name });
      if (typeof dest === "string") {
        await chat.saveFile(fileConv, dest);
        remember(fileConv, dest);
      }
    } catch (e) {
      handleSaveError(e);
    }
  };

  // A save failure handler that recognizes the already-downloaded case: chunks are pruned
  // after a successful save, so a later save attempt fails with "file incomplete" — that
  // means it was already downloaded (and reclaimed), not a real error.
  const handleSaveError = (e: unknown) => {
    const msg = errorMessage(e);
    setError(
      /incomplete/i.test(msg)
        ? t("files.alreadyDownloaded")
        : t("files.couldntSave", { error: msg }),
    );
  };

  const reveal = async (path: string) => {
    try {
      await revealItemInDir(path);
    } catch (e) {
      setError(t("files.couldntOpen", { error: errorMessage(e) }));
    }
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-action-files"
          title={t("files.received")}
          className="relative"
        >
          <Download className="h-4 w-4" />
          {files.length > 0 && (
            <Badge className="absolute -right-0.5 -top-0.5 h-4 min-w-4 justify-center bg-primary px-1 text-primary-foreground">
              {files.length}
            </Badge>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-0" data-testid="files-tray">
        <div className="flex items-center justify-between gap-2 border-b px-3 py-2.5">
          <span className="font-display text-sm font-semibold tracking-tight">
            {t("files.received")}
          </span>
          <button
            type="button"
            onClick={() => void chooseDir()}
            title={t("files.chooseFolder")}
            className="flex min-w-0 items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground hover:bg-accent"
          >
            <FolderOpen className="h-3.5 w-3.5 shrink-0" />
            <span className="max-w-32 truncate font-mono">
              {downloadDir || t("files.noDefaultFolder")}
            </span>
          </button>
        </div>
        {files.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-3 py-8 text-center">
            <Download className="h-7 w-7 text-muted-foreground/40" />
            <p className="text-sm text-muted-foreground">
              {t("files.nothingReceived")}
            </p>
          </div>
        ) : (
          <div className="max-h-72 overflow-y-auto p-1">
            {files.map((f) => (
              <div
                key={f.fileConv}
                className="rounded-lg px-2 py-1.5 hover:bg-accent/50"
              >
                <div className="flex items-center gap-2">
                  {fileGlyph(f.name)}
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm">{f.name}</div>
                    <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                      <IdentityGlyph
                        seed={f.fromName}
                        size={14}
                        className="shrink-0"
                        title={f.fromName}
                      />
                      <span className="truncate">{f.fromName}</span>
                      <span aria-hidden>·</span>
                      <span className="shrink-0 font-mono">
                        {humanSize(f.size)}
                      </span>
                    </div>
                  </div>
                  {savedPaths[f.fileConv] ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="h-7"
                      // Title shows WHERE it was saved; click reveals it in the file manager.
                      title={t("files.savedTo", {
                        path: savedPaths[f.fileConv],
                      })}
                      onClick={() => void reveal(savedPaths[f.fileConv])}
                    >
                      <FolderOpen className="h-3.5 w-3.5" />
                      {t("files.reveal")}
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="h-7"
                      onClick={() => void saveToDefault(f.fileConv, f.name)}
                    >
                      {t("common.save")}
                    </Button>
                  )}
                  <button
                    onClick={() => dismissFile(f.fileConv)}
                    className="rounded p-1 text-muted-foreground hover:bg-accent"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
                {!savedPaths[f.fileConv] && (
                  <button
                    type="button"
                    onClick={() => void saveAs(f.fileConv, f.name)}
                    className="mt-1 pl-6 text-xs text-muted-foreground hover:text-foreground hover:underline"
                  >
                    {t("files.saveAs")}
                  </button>
                )}
                <TransferBar transferKey={f.fileConv} />
              </div>
            ))}
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}
